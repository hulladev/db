//! Transactional jobs with fenced leases and at-least-once delivery.
use super::{Error, ManagedSchema, ManagedStore, WriteDb};
use serde_json::Value as Json;
use std::{future::Future, pin::Pin, sync::Mutex, time::Duration};
use tokio_postgres::{Row, Transaction};
use uuid::Uuid;

const MAX_BYTES: usize = 1024 * 1024;
const MAX_JOBS: usize = 100;
const MAX_DELAY: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAX_LEASE: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, Copy)]
pub struct EnqueueOptions {
    pub max_attempts: u32,
    pub delay: Duration,
}
impl Default for EnqueueOptions {
    fn default() -> Self {
        Self {
            max_attempts: 10,
            delay: Duration::ZERO,
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub struct ClaimOptions {
    pub limit: u16,
    pub lease: Duration,
}
impl Default for ClaimOptions {
    fn default() -> Self {
        Self {
            limit: 32,
            lease: Duration::from_secs(30),
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub struct WorkerOptions {
    pub lease: Duration,
    pub delivery_timeout: Duration,
    pub retry_delay: Duration,
}
impl Default for WorkerOptions {
    fn default() -> Self {
        Self {
            lease: Duration::from_secs(30),
            delivery_timeout: Duration::from_secs(10),
            retry_delay: Duration::from_secs(5),
        }
    }
}

pub(super) struct EnqueueContext<'a> {
    source: Uuid,
    request_id: Uuid,
    operation: &'a str,
    budget: Mutex<(usize, usize)>,
}
impl<'a> EnqueueContext<'a> {
    pub fn new(source: Uuid, request_id: Uuid, operation: &'a str) -> Self {
        Self {
            source,
            request_id,
            operation,
            budget: Mutex::new((0, 0)),
        }
    }
}

impl<I: Send + Sync + 'static> WriteDb<'_, I> {
    /// Enqueue trusted application work in the mutation transaction. The UUID
    /// remains stable through receipt replay and delivery retries. Payloads are
    /// JSON; use strings for int64 values. No external handler runs here.
    pub async fn enqueue(&self, name: &str, payload: Json) -> Result<Uuid, Error> {
        self.enqueue_with(name, payload, EnqueueOptions::default())
            .await
    }
    pub async fn enqueue_with(
        &self,
        name: &str,
        payload: Json,
        options: EnqueueOptions,
    ) -> Result<Uuid, Error> {
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            || !(1..=100).contains(&options.max_attempts)
        {
            return Err(Error::InvalidInput("outbox names require 1..128 ASCII letters, digits, dots, hyphens or underscores; attempts must be 1..100".into()));
        }
        let delay = duration_ms(options.delay, MAX_DELAY, false)?;
        let bytes = serde_json::to_vec(&payload)
            .map_err(|_| Error::CorruptData)?
            .len()
            .checked_add(name.len())
            .ok_or(Error::QuotaExceeded)?;
        {
            let mut budget = self.outbox.budget.lock().map_err(|_| Error::CorruptData)?;
            let total = budget.1.checked_add(bytes).ok_or(Error::QuotaExceeded)?;
            if budget.0 >= MAX_JOBS || total > MAX_BYTES {
                return Err(Error::QuotaExceeded);
            }
            *budget = (budget.0 + 1, total);
        }
        let id = Uuid::now_v7();
        self.read.tx.execute(&format!("INSERT INTO {} (id,source,scope,principal,authorization_scope,request_id,operation,name,payload,max_attempts,available_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,clock_timestamp()+$11::bigint*interval '1 millisecond')", self.read.schema.relation("outbox")),
            &[&id, &self.outbox.source, &self.context.scope, &self.context.principal, &self.context.authorization_scope, &self.outbox.request_id, &self.outbox.operation, &name, &payload, &(options.max_attempts as i32), &delay]).await?;
        // Jobs are private operational state, not query-visible table writes.
        Ok(id)
    }
}

/// A claimed job. The private token is required to change its lease or outcome.
/// A handler must use `id` as the external service's idempotency key.
#[derive(Debug, Clone)]
pub struct OutboxJob {
    pub id: Uuid,
    pub source: Uuid,
    pub scope: Uuid,
    pub principal: Uuid,
    pub authorization_scope: Uuid,
    pub request_id: Uuid,
    pub operation: String,
    pub name: String,
    pub payload: Json,
    pub attempt: u32,
    pub max_attempts: u32,
    token: Uuid,
}
/// Operator-visible retained work, including dead letters and old-source jobs.
#[derive(Debug, Clone)]
pub struct OutboxEntry {
    pub id: Uuid,
    pub source: Uuid,
    pub scope: Uuid,
    pub name: String,
    pub payload: Json,
    pub attempts: u32,
    pub max_attempts: u32,
    pub failed: bool,
    pub leased: bool,
    pub last_error: Option<String>,
}

impl ManagedStore {
    /// Claims only committed jobs for the current source. Concurrent workers
    /// skip locked rows; expired leases can be reclaimed with a new token.
    pub async fn claim_outbox(
        &mut self,
        expected_source: Uuid,
        options: ClaimOptions,
    ) -> Result<Vec<OutboxJob>, Error> {
        self.require_initialized()?;
        if !(1..=100).contains(&options.limit) {
            return Err(Error::InvalidInput(
                "outbox claim limit must be 1..100".into(),
            ));
        }
        let lease = duration_ms(options.lease, MAX_LEASE, true)?;
        let tx = self.client.transaction().await?;
        check_source(&tx, &self.schema, expected_source).await?;
        let relation = self.schema.relation("outbox");
        // A crash on the final attempt must eventually become a dead letter.
        tx.execute(&format!("UPDATE {relation} o SET failed=true,lease_token=NULL,lease_until=NULL,last_error=COALESCE(last_error,'delivery lease expired') FROM (SELECT id FROM {relation} WHERE source=$1 AND NOT failed AND attempts>=max_attempts AND (lease_until IS NULL OR lease_until<=clock_timestamp()) ORDER BY available_at,id LIMIT $2 FOR UPDATE SKIP LOCKED) expired WHERE o.id=expired.id"), &[&expected_source, &i64::from(options.limit)]).await?;
        let token = Uuid::now_v7();
        let rows = tx.query(&format!("UPDATE {relation} o SET attempts=o.attempts+1,lease_token=$3,lease_until=clock_timestamp()+$4::bigint*interval '1 millisecond' FROM (SELECT id FROM {relation} WHERE source=$1 AND NOT failed AND attempts<max_attempts AND available_at<=clock_timestamp() AND (lease_until IS NULL OR lease_until<=clock_timestamp()) ORDER BY available_at,id LIMIT $2 FOR UPDATE SKIP LOCKED) ready WHERE o.id=ready.id RETURNING o.id,o.source,o.scope,o.principal,o.authorization_scope,o.request_id,o.operation,o.name,o.payload,o.attempts,o.max_attempts,o.lease_token"), &[&expected_source, &i64::from(options.limit), &token, &lease]).await?;
        let jobs = rows.iter().map(decode_job).collect::<Result<Vec<_>, _>>()?;
        tx.commit().await.map_err(super::store::commit_error)?;
        Ok(jobs)
    }
    /// A stale or expired lease cannot acknowledge work claimed by a new owner.
    pub async fn acknowledge_outbox(&mut self, job: &OutboxJob) -> Result<(), Error> {
        self.require_initialized()?;
        let tx = self.client.transaction().await?;
        check_source(&tx, &self.schema, job.source).await?;
        let count = tx.execute(&format!("DELETE FROM {} WHERE id=$1 AND source=$2 AND lease_token=$3 AND lease_until>clock_timestamp() AND NOT failed", self.schema.relation("outbox")), &[&job.id, &job.source, &job.token]).await?;
        if count != 1 {
            return Err(Error::Conflict);
        }
        tx.commit().await.map_err(super::store::commit_error)?;
        Ok(())
    }
    /// Returns true if the job became a dead letter. Reasons must be redacted
    /// application diagnostics; at most 1024 UTF-8 bytes are retained.
    pub async fn retry_outbox(
        &mut self,
        job: &OutboxJob,
        delay: Duration,
        error: DeliveryError,
    ) -> Result<bool, Error> {
        self.require_initialized()?;
        let delay = duration_ms(delay, MAX_DELAY, false)?;
        let (permanent, reason) = match error {
            DeliveryError::Retry(reason) => (false, reason),
            DeliveryError::Permanent(reason) => (true, reason),
        };
        let reason = truncate(reason.replace('\0', "\\0"), 1024);
        let tx = self.client.transaction().await?;
        check_source(&tx, &self.schema, job.source).await?;
        let failed = tx.query_opt(&format!("UPDATE {} SET available_at=clock_timestamp()+$4::bigint*interval '1 millisecond',lease_token=NULL,lease_until=NULL,failed=(attempts>=max_attempts OR $5),last_error=$6 WHERE id=$1 AND source=$2 AND lease_token=$3 AND lease_until>clock_timestamp() AND NOT failed RETURNING failed", self.schema.relation("outbox")), &[&job.id, &job.source, &job.token, &delay, &permanent, &reason]).await?.ok_or(Error::Conflict)?.try_get(0)?;
        tx.commit().await.map_err(super::store::commit_error)?;
        Ok(failed)
    }
    pub async fn renew_outbox(&mut self, job: &OutboxJob, lease: Duration) -> Result<(), Error> {
        self.require_initialized()?;
        let lease = duration_ms(lease, MAX_LEASE, true)?;
        let tx = self.client.transaction().await?;
        check_source(&tx, &self.schema, job.source).await?;
        let count = tx.execute(&format!("UPDATE {} SET lease_until=clock_timestamp()+$4::bigint*interval '1 millisecond' WHERE id=$1 AND source=$2 AND lease_token=$3 AND lease_until>clock_timestamp() AND NOT failed", self.schema.relation("outbox")), &[&job.id, &job.source, &job.token, &lease]).await?;
        if count != 1 {
            return Err(Error::Conflict);
        }
        tx.commit().await.map_err(super::store::commit_error)?;
        Ok(())
    }
    /// Explicit operator reconciliation, also used to retry a dead letter.
    /// Preserves the job ID across source replacement. Active leases cannot be
    /// reset; stop old workers and wait for their leases before reconciling.
    pub async fn requeue_outbox(
        &mut self,
        id: Uuid,
        previous_source: Uuid,
        current_source: Uuid,
    ) -> Result<(), Error> {
        self.require_operator().await?;
        let tx = self.client.transaction().await?;
        check_source(&tx, &self.schema, current_source).await?;
        let count = tx.execute(&format!("UPDATE {} SET source=$3,attempts=0,available_at=clock_timestamp(),lease_token=NULL,lease_until=NULL,failed=false,last_error=NULL WHERE id=$1 AND source=$2 AND (lease_until IS NULL OR lease_until<=clock_timestamp())", self.schema.relation("outbox")), &[&id, &previous_source, &current_source]).await?;
        if count != 1 {
            return Err(Error::Conflict);
        }
        tx.commit().await.map_err(super::store::commit_error)?;
        Ok(())
    }
    /// Explicitly abandon retained work. Active deliveries cannot be recalled;
    /// stop workers and wait for their leases before discarding a job.
    pub async fn discard_outbox(
        &mut self,
        id: Uuid,
        previous_source: Uuid,
        current_source: Uuid,
    ) -> Result<(), Error> {
        self.require_operator().await?;
        let tx = self.client.transaction().await?;
        check_source(&tx, &self.schema, current_source).await?;
        let count = tx.execute(&format!("DELETE FROM {} WHERE id=$1 AND source=$2 AND (lease_until IS NULL OR lease_until<=clock_timestamp())", self.schema.relation("outbox")), &[&id, &previous_source]).await?;
        if count != 1 {
            return Err(Error::Conflict);
        }
        tx.commit().await.map_err(super::store::commit_error)?;
        Ok(())
    }
    pub async fn inspect_outbox(
        &mut self,
        scope: Uuid,
        limit: u16,
    ) -> Result<Vec<OutboxEntry>, Error> {
        self.require_operator().await?;
        if !(1..=100).contains(&limit) {
            return Err(Error::InvalidInput(
                "outbox inspection limit must be 1..100".into(),
            ));
        }
        let rows = self.client.query(&format!("SELECT id,source,scope,name,payload,attempts,max_attempts,failed,(lease_until>clock_timestamp()),last_error FROM {} WHERE scope=$1 ORDER BY available_at,id LIMIT $2", self.schema.relation("outbox")), &[&scope, &i64::from(limit)]).await?;
        rows.iter()
            .map(|row| {
                Ok(OutboxEntry {
                    id: row.try_get(0)?,
                    source: row.try_get(1)?,
                    scope: row.try_get(2)?,
                    name: row.try_get(3)?,
                    payload: row.try_get(4)?,
                    attempts: positive(row.try_get(5)?, false)?,
                    max_attempts: positive(row.try_get(6)?, true)?,
                    failed: row.try_get(7)?,
                    leased: row.try_get::<_, Option<bool>>(8)?.unwrap_or(false),
                    last_error: row.try_get(9)?,
                })
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub enum DeliveryError {
    Retry(String),
    Permanent(String),
}
pub type DeliveryFuture<'a> = Pin<Box<dyn Future<Output = Result<(), DeliveryError>> + Send + 'a>>;
pub trait OutboxHandler: Send + Sync {
    /// Execute outside the mutation transaction. Delivery may repeat after a
    /// timeout, cancellation or lost acknowledgement; deduplicate with job.id.
    fn deliver<'a>(&'a self, job: &'a OutboxJob) -> DeliveryFuture<'a>;
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Idle,
    Delivered(Uuid),
    RetryScheduled(Uuid),
    DeadLettered(Uuid),
}

/// A separate trusted process/task owns this worker. No job runs until the
/// application explicitly calls deliver_next; no network endpoint dispatches it.
pub struct OutboxWorker {
    store: ManagedStore,
    source: Uuid,
    options: WorkerOptions,
}
impl OutboxWorker {
    pub async fn connect(
        url: &str,
        schema: ManagedSchema,
        source: Uuid,
        options: WorkerOptions,
    ) -> Result<Self, Error> {
        duration_ms(options.lease, MAX_LEASE, true)?;
        duration_ms(options.retry_delay, MAX_DELAY, false)?;
        if options.delivery_timeout.is_zero() || options.delivery_timeout >= options.lease {
            return Err(Error::InvalidInput(
                "delivery timeout must be positive and shorter than the lease".into(),
            ));
        }
        let mut store = ManagedStore::connect(url, schema).await?;
        store.verify().await?;
        let tx = store.client.transaction().await?;
        check_source(&tx, &store.schema, source).await?;
        tx.commit().await?;
        Ok(Self {
            store,
            source,
            options,
        })
    }
    /// Claims one job, so sequential delivery cannot consume later jobs' leases.
    /// Cancellation leaves the lease to expire; no database lock spans delivery.
    pub async fn deliver_next(
        &mut self,
        handler: &dyn OutboxHandler,
    ) -> Result<DeliveryOutcome, Error> {
        let Some(job) = self
            .store
            .claim_outbox(
                self.source,
                ClaimOptions {
                    limit: 1,
                    lease: self.options.lease,
                },
            )
            .await?
            .pop()
        else {
            return Ok(DeliveryOutcome::Idle);
        };
        let result = tokio::time::timeout(self.options.delivery_timeout, handler.deliver(&job))
            .await
            .unwrap_or_else(|_| Err(DeliveryError::Retry("delivery deadline elapsed".into())));
        match result {
            Ok(()) => {
                self.store.acknowledge_outbox(&job).await?;
                Ok(DeliveryOutcome::Delivered(job.id))
            }
            Err(error) => {
                if self
                    .store
                    .retry_outbox(&job, self.options.retry_delay, error)
                    .await?
                {
                    Ok(DeliveryOutcome::DeadLettered(job.id))
                } else {
                    Ok(DeliveryOutcome::RetryScheduled(job.id))
                }
            }
        }
    }
}

fn decode_job(row: &Row) -> Result<OutboxJob, Error> {
    Ok(OutboxJob {
        id: row.try_get(0)?,
        source: row.try_get(1)?,
        scope: row.try_get(2)?,
        principal: row.try_get(3)?,
        authorization_scope: row.try_get(4)?,
        request_id: row.try_get(5)?,
        operation: row.try_get(6)?,
        name: row.try_get(7)?,
        payload: row.try_get(8)?,
        attempt: positive(row.try_get(9)?, true)?,
        max_attempts: positive(row.try_get(10)?, true)?,
        token: row.try_get(11)?,
    })
}
fn positive(value: i32, nonzero: bool) -> Result<u32, Error> {
    if value < i32::from(nonzero) || value > 100 {
        return Err(Error::CorruptData);
    }
    Ok(value as u32)
}
fn duration_ms(value: Duration, maximum: Duration, nonzero: bool) -> Result<i64, Error> {
    if value > maximum || (nonzero && value.as_millis() == 0) {
        return Err(Error::InvalidInput(
            "outbox duration is outside its bounded millisecond range".into(),
        ));
    }
    i64::try_from(value.as_millis())
        .map_err(|_| Error::InvalidInput("outbox duration overflow".into()))
}
fn truncate(mut value: String, max: usize) -> String {
    let mut end = value.len().min(max);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value
}
async fn check_source(
    tx: &Transaction<'_>,
    schema: &ManagedSchema,
    expected: Uuid,
) -> Result<(), Error> {
    // This SELECT holds ACCESS SHARE until commit, fencing rotate_source's
    // ACCESS EXCLUSIVE lock without granting workers metadata UPDATE rights.
    let source: Uuid = tx
        .query_one(
            &format!(
                "SELECT source FROM {} WHERE singleton",
                schema.relation("metadata")
            ),
            &[],
        )
        .await?
        .try_get(0)?;
    if source != expected {
        return Err(Error::SourceMismatch);
    }
    Ok(())
}

pub(super) async fn verify_layout(
    tx: &Transaction<'_>,
    schema: &ManagedSchema,
) -> Result<(), Error> {
    for (relation, expected) in [
        (
            "tickets",
            vec![
                "id:uuid",
                "scope:uuid",
                "principal:uuid",
                "authorization_scope:uuid",
                "source:uuid",
                "expires_at:timestamp with time zone",
            ],
        ),
        (
            "admissions",
            vec![
                "scope:uuid",
                "principal:uuid",
                "request_id:uuid",
                "ticket:uuid",
                "authorization_scope:uuid",
                "source:uuid",
                "operation:text",
                "input_digest:bytea",
                "status:text",
                "failure:text",
                "work_until:timestamp with time zone",
                "result_until:timestamp with time zone",
                "confirmation_ms:bigint",
                "grace_ms:bigint",
                "purge_after:timestamp with time zone",
            ],
        ),
        (
            "receipts",
            vec![
                "scope:uuid",
                "principal:uuid",
                "request_id:uuid",
                "authorization_scope:uuid",
                "operation:text",
                "input_digest:bytea",
                "receipt:jsonb",
                "created_at:timestamp with time zone",
                "expired:boolean",
            ],
        ),
        (
            "outbox",
            vec![
                "id:uuid",
                "source:uuid",
                "scope:uuid",
                "principal:uuid",
                "authorization_scope:uuid",
                "request_id:uuid",
                "operation:text",
                "name:text",
                "payload:jsonb",
                "max_attempts:integer",
                "attempts:integer",
                "available_at:timestamp with time zone",
                "lease_token:uuid",
                "lease_until:timestamp with time zone",
                "failed:boolean",
                "last_error:text",
            ],
        ),
        (
            "quotas",
            vec![
                "scope:uuid",
                "max_rows:bigint",
                "max_data_bytes:bigint",
                "max_receipt_bytes:bigint",
                "max_result_bytes:bigint",
                "max_outbox_jobs:bigint",
                "max_outbox_bytes:bigint",
            ],
        ),
    ] {
        let actual: Vec<String> = tx.query("SELECT attname::text || ':' || format_type(atttypid,atttypmod) FROM pg_attribute WHERE attrelid=to_regclass($1) AND attnum>0 AND NOT attisdropped ORDER BY attnum", &[&schema.relation(relation)]).await?.iter().map(|row| row.get(0)).collect();
        if actual != expected {
            return Err(Error::SchemaMismatch);
        }
    }
    Ok(())
}
