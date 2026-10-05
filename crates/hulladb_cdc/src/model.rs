use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Internal PostgreSQL position, stored losslessly as hex rather than signed SQL integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Lsn(pub u64);

impl fmt::Display for Lsn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:X}/{:X}", self.0 >> 32, self.0 & 0xffff_ffff)
    }
}

impl FromStr for Lsn {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Error> {
        let (hi, lo) = value.split_once('/').ok_or(Error::Invalid("invalid LSN"))?;
        let hi = u32::from_str_radix(hi, 16).map_err(|_| Error::Invalid("invalid LSN"))?;
        let lo = u32::from_str_radix(lo, 16).map_err(|_| Error::Invalid("invalid LSN"))?;
        Ok(Self((u64::from(hi) << 32) | u64::from(lo)))
    }
}

/// Caller-owned source generation. Reset/recreate of a slot requires a new generation/log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub generation: Uuid,
    pub system_id: String,
    pub database_oid: u32,
    pub publication_oid: u32,
    pub slot: String,
    pub publication: String,
    pub start: Lsn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub type_oid: u32,
    pub type_modifier: i32,
    pub key: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relation {
    pub oid: u32,
    pub namespace: String,
    pub name: String,
    pub replica_identity: u8,
    pub columns: Vec<Column>,
}

/// PostgreSQL text-format data. Type conversion belongs to a later schema adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Value {
    Null,
    Text(String),
    UnchangedToast,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Change {
    Insert {
        relation: Relation,
        row: Vec<Value>,
    },
    Update {
        relation: Relation,
        old: Option<Vec<Value>>,
        row: Vec<Value>,
    },
    Delete {
        relation: Relation,
        old: Vec<Value>,
    },
    Truncate {
        relations: Vec<Relation>,
        cascade: bool,
        restart_identity: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    pub xid: u32,
    pub commit: Lsn,
    pub end: Lsn,
    pub changes: Vec<Change>,
}

/// Scoped internal log cursor. It is not an authorization token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub generation: Uuid,
    pub sequence: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub cursor: Cursor,
    pub batch: Batch,
}

#[derive(Debug)]
pub enum Error {
    Invalid(&'static str),
    Protocol(&'static str),
    Limit,
    SourceMismatch,
    SourceUnavailable,
    SlotAhead,
    OwnerBusy,
    CursorExpired,
    InvalidCursor,
    Corrupt,
    Postgres(tokio_postgres::Error),
    Replication(pgwire_replication::error::PgWireError),
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(s) | Self::Protocol(s) => f.write_str(s),
            Self::Limit => f.write_str("CDC transaction or retention limit exceeded"),
            Self::SourceMismatch => {
                f.write_str("CDC source identity differs; create a new generation")
            }
            Self::SourceUnavailable => f.write_str(
                "replication slot is missing, invalidated, or too far ahead; bootstrap required",
            ),
            Self::SlotAhead => f.write_str("slot acknowledgement is ahead of durable local state"),
            Self::OwnerBusy => f.write_str("CDC slot already has an owner"),
            Self::CursorExpired => f.write_str("delivery history expired; snapshot required"),
            Self::InvalidCursor => f.write_str("cursor scope or sequence is invalid"),
            Self::Corrupt => f.write_str("CDC log/checkpoint invariant failed"),
            Self::Postgres(e) => fmt::Display::fmt(e, f),
            Self::Replication(e) => fmt::Display::fmt(e, f),
            Self::Sqlite(e) => fmt::Display::fmt(e, f),
            Self::Json(e) => fmt::Display::fmt(e, f),
        }
    }
}

impl std::error::Error for Error {}

impl From<tokio_postgres::Error> for Error {
    fn from(e: tokio_postgres::Error) -> Self {
        Self::Postgres(e)
    }
}
impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}
impl From<pgwire_replication::error::PgWireError> for Error {
    fn from(e: pgwire_replication::error::PgWireError) -> Self {
        Self::Replication(e)
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
