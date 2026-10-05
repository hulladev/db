use std::{fmt, sync::OnceLock};

use hulladb_schema::{
    FieldDefinition, RecordValues, ScalarType, TableContract, TableDeclaration, Value,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::GENERATION;

/// Trusted identity supplied by the caller's authentication boundary.
/// The development probe does not authenticate users or issue sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session {
    pub workspace: Uuid,
    pub principal: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Freshness token for an authoritative managed-query result, not a sync cursor.
pub struct QueryRevision {
    pub workspace: Uuid,
    pub generation: i32,
    pub sequence: i64,
}

impl QueryRevision {
    pub(crate) fn new(workspace: Uuid, sequence: i64) -> Self {
        Self {
            workspace,
            generation: GENERATION,
            sequence,
        }
    }

    pub(crate) fn validate(self, workspace: Uuid) -> Result<(), Error> {
        if self.workspace != workspace || self.generation != GENERATION || self.sequence < 0 {
            return Err(Error::InvalidQueryRevision);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub title: String,
    pub completed: bool,
    pub version: i64,
}

impl Task {
    pub fn values(&self) -> RecordValues {
        RecordValues::from([
            ("id".into(), Value::Uuid(*self.id.as_bytes())),
            ("title".into(), Value::String(self.title.clone())),
            ("completed".into(), Value::Boolean(self.completed)),
            ("version".into(), Value::Int64(self.version)),
        ])
    }

    pub(crate) fn validate(&self) -> Result<(), Error> {
        validate_title(&self.title)?;
        if self.version < 1 {
            return Err(Error::InvalidInput("version must be positive"));
        }
        task_contract()
            .validate_row(&self.values())
            .map_err(|_| Error::CorruptData)
    }
}

pub(crate) fn task_contract() -> &'static TableContract {
    static CONTRACT: OnceLock<TableContract> = OnceLock::new();
    CONTRACT.get_or_init(|| {
        TableDeclaration::new(
            "tasks",
            vec![
                FieldDefinition::new("title", ScalarType::String),
                FieldDefinition::new("completed", ScalarType::Boolean),
                FieldDefinition::new("version", ScalarType::Int64),
            ],
        )
        .normalize()
        .expect("the fixed task contract is valid")
    })
}

pub(crate) fn validate_title(title: &str) -> Result<(), Error> {
    if title.is_empty() || title.len() > 4096 || title.contains('\0') {
        return Err(Error::InvalidInput(
            "title must contain 1..4096 UTF-8 bytes without NUL",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    Create {
        id: Uuid,
        title: String,
        completed: bool,
    },
    Patch {
        id: Uuid,
        expected_version: i64,
        title: Option<String>,
        completed: Option<bool>,
    },
    Delete {
        id: Uuid,
        expected_version: i64,
    },
}

impl Command {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Create { title, .. } => validate_title(title),
            Self::Patch {
                expected_version,
                title,
                completed,
                ..
            } => {
                if *expected_version < 1 || (title.is_none() && completed.is_none()) {
                    return Err(Error::InvalidInput(
                        "patch needs a positive version and at least one field",
                    ));
                }
                if let Some(title) = title {
                    validate_title(title)?;
                }
                Ok(())
            }
            Self::Delete {
                expected_version, ..
            } if *expected_version < 1 => {
                Err(Error::InvalidInput("expected version must be positive"))
            }
            Self::Delete { .. } => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Change {
    Upsert(Task),
    Delete(Uuid),
}

/// A committed command result. Its revision invalidates query caches; it is
/// not a resumable CDC cursor and must never be applied as a sync event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub request_id: Uuid,
    pub revision: QueryRevision,
    pub change: Change,
}

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
                f.write_str("query revision scope, generation, or sequence is invalid")
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
