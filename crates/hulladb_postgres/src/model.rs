use std::fmt;

use uuid::Uuid;

pub use hulladb_protocol::{Change, Command, QueryRevision, Receipt, Task};

#[derive(Debug, Clone)]
pub struct Page {
    pub tasks: Vec<Task>,
    /// Continue with the same workspace and completed filter. Pages are separate snapshots.
    pub next: Option<Uuid>,
}

#[derive(Debug)]
pub enum Error {
    Forbidden,
    NotFound,
    Conflict,
    RequestIdReused,
    InvalidInput(&'static str),
    InvalidQueryRevision,
    QueryNotCovered,
    CorruptData,
    Database(tokio_postgres::Error),
    /// Recover with the same request ID; never assume the transaction failed.
    CommitOutcomeUnknown,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Forbidden => f.write_str("operation is not authorized"),
            Self::NotFound => f.write_str("task does not exist"),
            Self::Conflict => f.write_str("task identity or expected version conflicts"),
            Self::RequestIdReused => f.write_str("request ID was reused with different arguments"),
            Self::InvalidInput(reason) => f.write_str(reason),
            Self::InvalidQueryRevision => {
                f.write_str("query revision scope or sequence is invalid")
            }
            Self::QueryNotCovered => {
                f.write_str("this cache does not establish coverage for that query")
            }
            Self::CorruptData => f.write_str("stored task or receipt data violates its contract"),
            Self::Database(error) => write!(f, "PostgreSQL operation failed: {error}"),
            Self::CommitOutcomeUnknown => {
                f.write_str("commit outcome unknown; recover the existing request")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            _ => None,
        }
    }
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Database(error)
    }
}

impl From<hulladb_protocol::ContractError> for Error {
    fn from(error: hulladb_protocol::ContractError) -> Self {
        Self::InvalidInput(error.0)
    }
}
