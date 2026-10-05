use std::time::Duration;

use pgwire_replication::{ReplicationClient, ReplicationConfig, ReplicationEvent};
use tokio::task::JoinHandle;
use tokio_postgres::{Client, NoTls};

use crate::{Cursor, Error, EventLog, Lsn, Source, pgoutput::Decoder};

/// Cooperative SQL ownership plus a persistent pgoutput v1 replication connection.
pub struct Capture {
    pub(crate) db: Client,
    connection: JoinHandle<Result<(), tokio_postgres::Error>>,
    slot: String,
    config: ReplicationConfig,
    stream: Option<ReplicationClient>,
    decoder: Decoder,
    resetting: bool,
    feedback: Lsn,
    persisted: Lsn,
    pub(crate) source: Source,
}

#[derive(Debug)]
pub struct Progress {
    pub appended: Vec<Cursor>,
    pub replayed: usize,
    pub checkpoint: Lsn,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop_stream();
        self.connection.abort();
    }
}

impl Capture {
    /// Local development only. Does not create/drop slots or bootstrap existing rows.
    pub async fn connect(url: &str, source: &Source) -> Result<Self, Error> {
        name(&source.slot)?;
        name(&source.publication)?;
        let mut config: tokio_postgres::Config = url.parse()?;
        config.connect_timeout(Duration::from_secs(5));
        let replication = replication_config(&config, source)?;
        let (db, connection) = config.connect(NoTls).await?;
        let capture = Self {
            db,
            connection: tokio::spawn(connection),
            slot: source.slot.clone(),
            config: replication,
            stream: None,
            decoder: Decoder::default(),
            resetting: false,
            feedback: source.start,
            persisted: source.start,
            source: source.clone(),
        };
        capture
            .db
            .batch_execute("SET statement_timeout='5s'; SET lock_timeout='5s'; SET DateStyle='ISO, YMD'; SET IntervalStyle='postgres'; SET TimeZone='UTC'; SET extra_float_digits=3; SET bytea_output='hex'")
            .await?;
        let owner: bool = capture
            .db
            .query_one(
                "SELECT pg_try_advisory_lock(hashtextextended($1,782116235))",
                &[&source.slot],
            )
            .await?
            .get(0);
        if !owner {
            return Err(Error::OwnerBusy);
        }
        let row = capture.db.query_one("SELECT (pg_control_system()).system_identifier::text, (SELECT oid FROM pg_database WHERE datname=current_database()), (SELECT oid FROM pg_publication WHERE pubname=$1), current_setting('server_encoding'), current_setting('wal_level')", &[&source.publication]).await?;
        let publication_oid: Option<u32> = row.get(2);
        if row.get::<_, String>(0) != source.system_id
            || row.get::<_, u32>(1) != source.database_oid
            || publication_oid != Some(source.publication_oid)
        {
            return Err(Error::SourceMismatch);
        }
        if row.get::<_, String>(3) != "UTF8" || row.get::<_, String>(4) != "logical" {
            return Err(Error::Invalid("CDC requires logical WAL and UTF8"));
        }
        Ok(capture)
    }

    pub(crate) async fn check_slot(&self, checkpoint: Lsn) -> Result<(), Error> {
        let row = self.db.query_opt("SELECT plugin,database,temporary,active,restart_lsn::text,confirmed_flush_lsn::text,wal_status FROM pg_replication_slots WHERE slot_name=$1 AND database=current_database()", &[&self.slot]).await?.ok_or(Error::SourceUnavailable)?;
        if row.get::<_, String>(0) != "pgoutput" || row.get::<_, bool>(2) {
            return Err(Error::SourceMismatch);
        }
        if row.get::<_, bool>(3) {
            return Err(Error::OwnerBusy);
        }
        let restart: Option<String> = row.get(4);
        let confirmed: Option<String> = row.get(5);
        let status: Option<String> = row.get(6);
        if status.as_deref() == Some("lost") || restart.is_none() || confirmed.is_none() {
            return Err(Error::SourceUnavailable);
        }
        if restart.unwrap().parse::<Lsn>()? > checkpoint {
            return Err(Error::SourceUnavailable);
        }
        if confirmed.unwrap().parse::<Lsn>()? > checkpoint {
            return Err(Error::SlotAhead);
        }
        Ok(())
    }

    /// Await one complete durable transaction or an idle decoding checkpoint.
    /// Feedback is asynchronous and reports only the durable log checkpoint.
    /// Cancellation while waiting preserves decoder state for the next call.
    pub async fn next(&mut self, log: &mut EventLog) -> Result<Progress, Error> {
        let progress = self.persist_next(log).await?;
        self.stream
            .as_ref()
            .ok_or(Error::SourceUnavailable)?
            .update_applied_lsn(pgwire_replication::Lsn::from_u64(progress.checkpoint.0));
        self.feedback = progress.checkpoint;
        Ok(progress)
    }

    /// Persist the next stream boundary without advancing feedback. Exposes the
    /// durable-before-feedback crash window; reconnect replays unacknowledged work.
    pub async fn persist_next(&mut self, log: &mut EventLog) -> Result<Progress, Error> {
        if log.source() != &self.source {
            return Err(Error::SourceMismatch);
        }
        if log.checkpoint()? < self.persisted {
            return Err(Error::SlotAhead);
        }
        let result = self.receive(log).await;
        if let Ok(progress) = &result {
            self.persisted = progress.checkpoint;
        }
        if result.is_err() {
            // A consumed frame cannot be retried in place after decode/storage
            // failure. Stop immediately, discard partial state, and replay from
            // the upstream acknowledgement on the next call.
            self.stop_stream();
        }
        result
    }

    async fn receive(&mut self, log: &mut EventLog) -> Result<Progress, Error> {
        if self.stream.is_none() {
            if self.resetting {
                self.wait_inactive().await?;
                self.resetting = false;
            }
            self.check_slot(log.checkpoint()?).await?;
            let confirmed: String = self
                .db
                .query_one(
                    "SELECT confirmed_flush_lsn::text FROM pg_replication_slots WHERE slot_name=$1",
                    &[&self.slot],
                )
                .await?
                .get(0);
            let start: Lsn = confirmed.parse()?;
            if start > log.checkpoint()? {
                return Err(Error::SlotAhead);
            }
            // Keep the worker's initial feedback at the upstream acknowledgement,
            // even when local durable storage is ahead after a crash.
            self.stream = Some(
                ReplicationClient::connect(
                    self.config
                        .clone()
                        .with_start_lsn(pgwire_replication::Lsn::from_u64(start.0)),
                )
                .await?,
            );
        }
        loop {
            let event = self
                .stream
                .as_mut()
                .unwrap()
                .recv()
                .await?
                .ok_or(Error::SourceUnavailable)?;
            let batch = match event {
                ReplicationEvent::Begin {
                    final_lsn,
                    xid,
                    commit_time_micros,
                } => {
                    let mut bytes = Vec::with_capacity(21);
                    bytes.push(b'B');
                    bytes.extend(final_lsn.as_u64().to_be_bytes());
                    bytes.extend(commit_time_micros.to_be_bytes());
                    bytes.extend(xid.to_be_bytes());
                    self.decoder.push(&bytes)?
                }
                ReplicationEvent::Commit {
                    lsn,
                    end_lsn,
                    commit_time_micros,
                } => {
                    let mut bytes = Vec::with_capacity(26);
                    bytes.extend([b'C', 0]);
                    bytes.extend(lsn.as_u64().to_be_bytes());
                    bytes.extend(end_lsn.as_u64().to_be_bytes());
                    bytes.extend(commit_time_micros.to_be_bytes());
                    self.decoder.push(&bytes)?
                }
                ReplicationEvent::XLogData { data, .. } => {
                    // XLogData.wal_end is a server position, not proof that a
                    // complete transaction has reached durable storage.
                    self.decoder.push(&data)?
                }
                ReplicationEvent::Message {
                    transactional,
                    prefix,
                    content,
                    ..
                } => {
                    // The transport enables messages. This row-change contract
                    // ignores their contents, but still charges their size.
                    self.decoder.ignore_message(
                        transactional,
                        prefix
                            .len()
                            .checked_add(content.len())
                            .and_then(|s| s.checked_add(15))
                            .ok_or(Error::Limit)?,
                    )?;
                    None
                }
                ReplicationEvent::KeepAlive { wal_end, .. } => {
                    // PostgreSQL logical walsender sends its decoded sentPtr in
                    // idle keepalives. Ordered receipt outside a transaction is
                    // a scan boundary; an interleaved heartbeat is not.
                    if self.decoder.in_transaction() {
                        continue;
                    }
                    let checkpoint = Lsn(wal_end.as_u64());
                    log.scanned_through(checkpoint)?;
                    return Ok(Progress {
                        appended: Vec::new(),
                        replayed: 0,
                        checkpoint: log.checkpoint()?,
                    });
                }
                ReplicationEvent::StoppedAt { .. } => return Err(Error::SourceUnavailable),
            };
            if let Some(batch) = batch {
                if batch.changes.is_empty() {
                    log.scanned_through(batch.end)?;
                    return Ok(Progress {
                        appended: Vec::new(),
                        replayed: 0,
                        checkpoint: log.checkpoint()?,
                    });
                }
                let cursor = log.append(&batch)?;
                return Ok(Progress {
                    appended: cursor.into_iter().collect(),
                    replayed: usize::from(cursor.is_none()),
                    checkpoint: log.checkpoint()?,
                });
            }
        }
    }

    fn stop_stream(&mut self) {
        if let Some(mut stream) = self.stream.take() {
            stream.abort();
            self.resetting = true;
        }
        self.decoder = Decoder::default();
    }

    async fn wait_inactive(&self) -> Result<(), Error> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let active = self
                .db
                .query_opt(
                    "SELECT active FROM pg_replication_slots WHERE slot_name=$1",
                    &[&self.slot],
                )
                .await?
                .is_some_and(|r| r.get::<_, bool>(0));
            if !active {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::OwnerBusy);
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    /// Dedicated connection close releases cooperative slot ownership.
    pub async fn close(mut self) -> Result<(), Error> {
        if self.stream.is_some() {
            // Graceful close waits for feedback already authorized by `next`.
            // `persist_next` alone does not advance this target.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                let confirmed: String = self.db.query_one(
                    "SELECT confirmed_flush_lsn::text FROM pg_replication_slots WHERE slot_name=$1",
                    &[&self.slot],
                ).await?.get(0);
                if confirmed.parse::<Lsn>()? >= self.feedback {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(Error::SourceUnavailable);
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        if let Some(mut stream) = self.stream.take() {
            match tokio::time::timeout(Duration::from_secs(5), stream.shutdown()).await {
                Ok(result) => result?,
                Err(_) => {
                    stream.abort();
                    return Err(Error::SourceUnavailable);
                }
            }
            self.resetting = true;
        }
        if self.resetting {
            self.wait_inactive().await?;
        }
        self.db
            .query_one(
                "SELECT pg_advisory_unlock(hashtextextended($1,782116235))",
                &[&self.slot],
            )
            .await?;
        Ok(())
    }
}

fn name(value: &str) -> Result<(), Error> {
    if value.is_empty()
        || value.len() > 63
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(Error::Invalid(
            "CDC identifiers must use lowercase ASCII letters, digits, or underscores",
        ));
    }
    Ok(())
}

// Match the dedicated SQL connection exactly. Multi-host/failover configuration
// needs a coordinated source check on the replication connection before support.
fn replication_config(
    config: &tokio_postgres::Config,
    source: &Source,
) -> Result<ReplicationConfig, Error> {
    if config.get_hosts().len() != 1
        || !config.get_hostaddrs().is_empty()
        || config.get_ports().len() > 1
        || config.get_ssl_mode() == tokio_postgres::config::SslMode::Require
    {
        return Err(Error::Invalid(
            "CDC streaming requires one host and non-TLS development transport",
        ));
    }
    let host = match &config.get_hosts()[0] {
        tokio_postgres::config::Host::Tcp(host) => host.clone(),
        #[cfg(unix)]
        tokio_postgres::config::Host::Unix(path) => path
            .to_str()
            .ok_or(Error::Invalid("non-UTF8 socket path"))?
            .to_owned(),
    };
    let user = config
        .get_user()
        .ok_or(Error::Invalid("CDC requires an explicit user"))?;
    let password = std::str::from_utf8(config.get_password().unwrap_or_default())
        .map_err(|_| Error::Invalid("non-UTF8 password"))?;
    let database = config.get_dbname().unwrap_or(user);
    let port = config.get_ports().first().copied().unwrap_or(5432);
    Ok(ReplicationConfig::new(host, user, password, database, &source.slot, source.publication.clone())
        .with_port(port)
        .with_buffer_size(32)
        .with_status_interval(Duration::from_millis(100))
        .with_wakeup_interval(Duration::from_millis(100))
        .with_options(format!("{} -c DateStyle=ISO,YMD -c IntervalStyle=postgres -c TimeZone=UTC -c extra_float_digits=3 -c bytea_output=hex", config.get_options().unwrap_or_default())))
}
