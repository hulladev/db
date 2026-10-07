//! Portable contracts for the fixed task serving slice. No database credentials,
//! PostgreSQL positions, physical relation definitions, or backend drivers.
use std::{fmt, sync::OnceLock};

use hulladb_schema::{
    FieldDefinition, RecordValues, ScalarType, TableContract, TableDeclaration, Value,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Existing task-query contract; deployment query budgets are a separate concern.
pub const MAX_PAGE: u16 = 100;

/// Identity supplied by trusted authentication. This value is not an auth grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub workspace: Uuid,
    pub principal: Uuid,
}

/// Exact summary query, ordered by UUID with an exclusive keyset bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskQuery {
    pub completed: Option<bool>,
    pub after: Option<Uuid>,
    pub limit: u16,
}
impl TaskQuery {
    pub fn validate(self) -> Result<(), ContractError> {
        if self.limit == 0 || self.limit > MAX_PAGE {
            return Err(ContractError("query limit must be 1..100"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSummary {
    pub id: Uuid,
    pub title: String,
    pub completed: bool,
}
impl TaskSummary {
    pub fn values(&self) -> RecordValues {
        RecordValues::from([
            ("id".into(), Value::Uuid(*self.id.as_bytes())),
            ("title".into(), Value::String(self.title.clone())),
            ("completed".into(), Value::Boolean(self.completed)),
        ])
    }

    /// Scalar shape is enforced by Rust/Serde; the title has a domain constraint.
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_title(&self.title)
    }
}

pub fn validate_title(title: &str) -> Result<(), ContractError> {
    if title.is_empty() || title.len() > 4096 || title.contains('\0') {
        return Err(ContractError(
            "title must contain 1..4096 UTF-8 bytes without NUL",
        ));
    }
    Ok(())
}

/// Orders replacements in one authenticated query lifecycle. This is neither a
/// PostgreSQL WAL position nor a receipt revision, and does not measure CDC lag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryCursor {
    pub subscription: Uuid,
    pub sequence: i64,
}

/// A query snapshot at or beyond this managed commit includes its effects in
/// query evaluation, even when filtering or top-N membership excludes the row.
/// This is scoped progress, not a delivery cursor or a grant of authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisibilityFence {
    pub source: Uuid,
    pub workspace: Uuid,
    pub sequence: i64,
}
impl VisibilityFence {
    pub fn validate(self, workspace: Uuid) -> Result<(), ContractError> {
        if self.workspace != workspace || self.sequence < 0 {
            return Err(ContractError(
                "visibility fence scope or sequence is invalid",
            ));
        }
        Ok(())
    }
}

/// Internal delivery contract; authenticated network framing remains separate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskMessage {
    pub session: Session,
    pub query: TaskQuery,
    pub cursor: DeliveryCursor,
    /// Revision paired with snapshot rows. Terminal results establish no coverage.
    pub visibility: VisibilityFence,
    pub result: TaskResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskResult {
    /// Complete for this exact query. Can coalesce earlier replacements.
    Snapshot {
        tasks: Vec<TaskSummary>,
    },
    Revoked,
    ResetRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContractError(pub &'static str);
impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for ContractError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Freshness token for an authoritative managed-query result, not a sync cursor.
pub struct QueryRevision {
    pub workspace: Uuid,
    pub sequence: i64,
}

impl QueryRevision {
    pub fn new(workspace: Uuid, sequence: i64) -> Self {
        Self {
            workspace,
            sequence,
        }
    }

    pub fn validate(self, workspace: Uuid) -> Result<(), ContractError> {
        if self.workspace != workspace || self.sequence < 0 {
            return Err(ContractError("query revision scope or sequence is invalid"));
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

    pub fn validate(&self) -> Result<(), ContractError> {
        validate_title(&self.title)?;
        if self.version < 1 {
            return Err(ContractError("version must be positive"));
        }
        task_contract()
            .validate_row(&self.values())
            .map_err(|_| ContractError("task data violates its contract"))
    }
}

fn task_contract() -> &'static TableContract {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    pub fn validate(&self) -> Result<(), ContractError> {
        match self {
            Self::Create { title, .. } => validate_title(title),
            Self::Patch {
                expected_version,
                title,
                completed,
                ..
            } => {
                if *expected_version < 1 || (title.is_none() && completed.is_none()) {
                    return Err(ContractError(
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
            } if *expected_version < 1 => Err(ContractError("expected version must be positive")),
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
/// not a delivery cursor and must never be applied as a sync event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub request_id: Uuid,
    /// Stored with the original commit; recovery must not rebind it after reset.
    pub source: Uuid,
    pub revision: QueryRevision,
    pub change: Change,
}

impl Receipt {
    pub fn visibility_fence(&self) -> VisibilityFence {
        VisibilityFence {
            source: self.source,
            workspace: self.revision.workspace,
            sequence: self.revision.sequence,
        }
    }
}

/// A stable request ID belongs to the caller's authenticated scope. Identity is
/// never accepted in a mutation body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRequest {
    pub request_id: Uuid,
    pub command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiErrorCode {
    Unauthorized,
    Forbidden,
    InvalidRequest,
    NotFound,
    Conflict,
    RequestIdReused,
    Capacity,
    Unavailable,
    ResetRequired,
    /// Recover or retry with the original request ID; never assume rollback.
    OutcomeUnknown,
}

/// Public errors carry no SQL, credentials, or database diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiError {
    pub code: ApiErrorCode,
}
