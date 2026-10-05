//! A bounded command connection for slot-exported snapshots. Streaming remains
//! in pgwire-replication; this connection never consumes or acknowledges WAL.
use std::time::Duration;

use pgwire_replication::{
    ReplicationConfig,
    auth::ScramClient,
    error::PgWireError,
    protocol::{
        framing::{write_password_message, write_query, write_startup_message},
        parse_auth_request, parse_error_response,
    },
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};

use crate::{Error, Lsn, Source};

const MAX_FRAME_BYTES: usize = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(5);
trait Socket: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Socket for T {}

pub(crate) struct ExportConnection {
    socket: Box<dyn Socket>,
}

impl ExportConnection {
    pub(crate) async fn connect(
        config: &ReplicationConfig,
        source: &Source,
    ) -> Result<Self, Error> {
        bounded(async {
            let socket: Box<dyn Socket> = if config.host.starts_with('/') {
                #[cfg(unix)]
                {
                    let path = std::path::Path::new(&config.host)
                        .join(format!(".s.PGSQL.{}", config.port));
                    Box::new(tokio::net::UnixStream::connect(path).await.map_err(PgWireError::from)?)
                }
                #[cfg(not(unix))]
                return Err(Error::Invalid("Unix socket bootstrap is unsupported on this platform"));
            } else {
                let socket = tokio::net::TcpStream::connect((config.host.as_str(), config.port))
                    .await.map_err(PgWireError::from)?;
                socket.set_nodelay(true).map_err(PgWireError::from)?;
                Box::new(socket)
            };
            let mut connection = Self { socket };
            let options = format!("{} -c statement_timeout=5000 -c lock_timeout=5000", config.options.as_deref().unwrap_or_default());
            write_startup_message(&mut connection.socket, 196608, &[
                ("user", &config.user), ("database", &config.database),
                ("replication", "database"), ("application_name", "hulladb-bootstrap"),
                ("client_encoding", "UTF8"), ("options", &options),
            ]).await?;
            connection.authenticate(config).await?;
            // Check the command connection too, before creating a persistent slot.
            let identity = connection.query(&format!(
                "SELECT (pg_control_system()).system_identifier::text, (SELECT oid::text FROM pg_database WHERE datname=current_database()), (SELECT oid::text FROM pg_publication WHERE pubname='{}')", source.publication,
            ), 3).await?;
            if identity[0] != source.system_id
                || identity[1] != source.database_oid.to_string()
                || identity[2] != source.publication_oid.to_string()
            {
                return Err(Error::SourceMismatch);
            }
            Ok(connection)
        }).await
    }

    pub(crate) async fn export(&mut self, slot: &str) -> Result<(Lsn, String), Error> {
        let values = bounded(self.query(
            &format!("CREATE_REPLICATION_SLOT {slot} LOGICAL pgoutput (SNAPSHOT 'export')"),
            4,
        ))
        .await?;
        if values[0] != slot || values[3] != "pgoutput" {
            return Err(Error::Protocol("unexpected slot export result"));
        }
        let start = values[1].parse::<Lsn>()?;
        if start == Lsn(0) || !valid_snapshot_name(&values[2]) {
            return Err(Error::Protocol("invalid exported snapshot identity"));
        }
        Ok((start, values[2].clone()))
    }

    async fn authenticate(&mut self, config: &ReplicationConfig) -> Result<(), Error> {
        let mut authenticated = false;
        loop {
            let (tag, payload) = self.read().await?;
            match tag {
                b'R' if !authenticated => {
                    let (code, data) = parse_auth_request(&payload)?;
                    match code {
                        0 if data.is_empty() => authenticated = true,
                        3 => {
                            let mut password = config.password.as_bytes().to_vec();
                            password.push(0);
                            write_password_message(&mut self.socket, &password).await?;
                        }
                        10 => {
                            if !data
                                .split(|b| *b == 0)
                                .any(|mechanism| mechanism == b"SCRAM-SHA-256")
                            {
                                return Err(PgWireError::Auth(
                                    "SCRAM-SHA-256 is unavailable".into(),
                                )
                                .into());
                            }
                            let scram = ScramClient::new(&config.user);
                            let mut initial = b"SCRAM-SHA-256\0".to_vec();
                            initial.extend((scram.client_first.len() as i32).to_be_bytes());
                            initial.extend(scram.client_first.as_bytes());
                            write_password_message(&mut self.socket, &initial).await?;
                            let first = self.auth_data(11).await?;
                            let (final_message, auth_message, salted_password) =
                                scram.client_final(&config.password, &first)?;
                            write_password_message(&mut self.socket, final_message.as_bytes())
                                .await?;
                            let final_reply = self.auth_data(12).await?;
                            ScramClient::verify_server_final(
                                &final_reply,
                                &salted_password,
                                &auth_message,
                            )?;
                        }
                        _ => {
                            return Err(PgWireError::Auth(
                                "unsupported bootstrap authentication".into(),
                            )
                            .into());
                        }
                    }
                }
                b'S' | b'K' | b'N' => {}
                b'Z' if authenticated && payload == b"I" => return Ok(()),
                _ => return Err(Error::Protocol("unexpected bootstrap authentication frame")),
            }
        }
    }

    async fn auth_data(&mut self, expected: i32) -> Result<String, Error> {
        let (tag, payload) = self.read().await?;
        if tag != b'R' {
            return Err(Error::Protocol("expected bootstrap authentication data"));
        }
        let (code, data) = parse_auth_request(&payload)?;
        if code != expected {
            return Err(Error::Protocol("unexpected bootstrap authentication phase"));
        }
        Ok(std::str::from_utf8(data)
            .map_err(|_| Error::Protocol("non-UTF8 authentication data"))?
            .into())
    }

    async fn query(&mut self, sql: &str, fields: usize) -> Result<Vec<String>, Error> {
        write_query(&mut self.socket, sql).await?;
        let mut description = false;
        let mut completed = false;
        let mut row = None;
        loop {
            let (tag, payload) = self.read().await?;
            match tag {
                b'T' if !description && !completed => {
                    if payload.len() < 2
                        || usize::from(u16::from_be_bytes([payload[0], payload[1]])) != fields
                    {
                        return Err(Error::Protocol("unexpected bootstrap result columns"));
                    }
                    description = true;
                }
                b'D' if description && row.is_none() && !completed => {
                    row = Some(parse_row(&payload, fields)?)
                }
                b'C' if row.is_some() && !completed => completed = true,
                b'N' | b'S' => {}
                b'Z' if completed && payload == b"I" => {
                    return row.ok_or(Error::Protocol("missing bootstrap result"));
                }
                _ => return Err(Error::Protocol("unexpected bootstrap query frame")),
            }
        }
    }

    async fn read(&mut self) -> Result<(u8, Vec<u8>), Error> {
        let frame = read_frame(&mut self.socket).await?;
        if frame.0 == b'E' {
            return Err(PgWireError::Server(parse_error_response(&frame.1)).into());
        }
        Ok(frame)
    }
}

async fn bounded<T>(
    operation: impl std::future::Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    tokio::time::timeout(TIMEOUT, operation)
        .await
        .map_err(|_| {
            Error::Replication(PgWireError::from(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "snapshot bootstrap timed out; inspect the owned slot before retrying",
            )))
        })?
}

async fn read_frame(reader: &mut (impl AsyncRead + Unpin)) -> Result<(u8, Vec<u8>), Error> {
    let mut header = [0; 5];
    reader
        .read_exact(&mut header)
        .await
        .map_err(PgWireError::from)?;
    let length = i32::from_be_bytes(header[1..].try_into().unwrap());
    if length < 4 || length as usize > MAX_FRAME_BYTES {
        return Err(Error::Protocol("bootstrap frame exceeds its length bound"));
    }
    let mut payload = vec![0; length as usize - 4];
    reader
        .read_exact(&mut payload)
        .await
        .map_err(PgWireError::from)?;
    Ok((header[0], payload))
}

fn valid_snapshot_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

fn parse_row(mut payload: &[u8], fields: usize) -> Result<Vec<String>, Error> {
    if payload.len() < 2 || usize::from(u16::from_be_bytes([payload[0], payload[1]])) != fields {
        return Err(Error::Protocol("invalid bootstrap row columns"));
    }
    payload = &payload[2..];
    let mut values = Vec::with_capacity(fields);
    for _ in 0..fields {
        if payload.len() < 4 {
            return Err(Error::Protocol("truncated bootstrap row"));
        }
        let length = i32::from_be_bytes(payload[..4].try_into().unwrap());
        payload = &payload[4..];
        if length < 0 || length as usize > payload.len() {
            return Err(Error::Protocol("null or truncated bootstrap field"));
        }
        let (value, rest) = payload.split_at(length as usize);
        values.push(
            std::str::from_utf8(value)
                .map_err(|_| Error::Protocol("non-UTF8 bootstrap field"))?
                .into(),
        );
        payload = rest;
    }
    if !payload.is_empty() {
        return Err(Error::Protocol("trailing bootstrap row bytes"));
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bootstrap_frames_reject_oversized_and_truncated_inputs_before_allocation() {
        for length in [-1, 3, (MAX_FRAME_BYTES + 1) as i32] {
            let mut bytes = vec![b'D'];
            bytes.extend(length.to_be_bytes());
            assert!(matches!(
                read_frame(&mut bytes.as_slice()).await,
                Err(Error::Protocol(_))
            ));
        }
        assert!(
            read_frame(&mut [b'D', 0, 0, 0, 8, 1].as_slice())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_ready_frame_cannot_skip_bootstrap_authentication() {
        let (socket, mut server) = tokio::io::duplex(32);
        use tokio::io::AsyncWriteExt;
        server.write_all(&[b'Z', 0, 0, 0, 5, b'I']).await.unwrap();
        let mut connection = ExportConnection {
            socket: Box::new(socket),
        };
        assert!(matches!(
            connection.authenticate(&ReplicationConfig::default()).await,
            Err(Error::Protocol(_))
        ));
    }

    #[test]
    fn export_rows_reject_nulls_truncation_extra_fields_and_unsafe_snapshot_names() {
        let mut row = vec![0, 1];
        row.extend(3_i32.to_be_bytes());
        row.extend(b"abc");
        assert_eq!(parse_row(&row, 1).unwrap(), ["abc"]);
        for length in 0..row.len() {
            assert!(parse_row(&row[..length], 1).is_err());
        }
        let mut trailing = row.clone();
        trailing.push(0);
        assert!(parse_row(&trailing, 1).is_err());
        assert!(parse_row(&[0, 1, 255, 255, 255, 255], 1).is_err());
        assert!(parse_row(&row, 2).is_err());
        assert!(valid_snapshot_name("00000003-0000001B-1"));
        for name in ["", "snapshot';SELECT 1", "a\\b", "é"] {
            assert!(!valid_snapshot_name(name));
        }
    }
}
