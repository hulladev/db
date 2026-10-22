//! Schema-driven PostgreSQL execution.
//!
//! All functions execute through a registry. Queries and authorization receive
//! only snapshot reads; mutations share one transaction for data, revision, and
//! receipt. This internal runtime uses canonical records, not generated Rust
//! application types or an HTTP/SSE protocol. See docs/managed-functions.md.

mod admission;
mod aggregate;
pub use aggregate::{Aggregate, AggregateOp, AggregateQuery};
pub use hulladb_protocol::ApplicationError;
mod concurrency;
mod db;
mod write_expression;
pub use db::{InsertConflict, InsertConflictAction};
pub use write_expression::{WriteExpression, WriteSource, WriteValues};
mod predicate;
pub use predicate::{Direction, FieldPath, Operand, Order, Predicate};
mod direct;
mod functions;
mod input_digest;
mod operations;
mod outbox;
mod pagination;
mod query_round;
mod storage;
mod store;
mod tls;
#[cfg(feature = "typescript")]
pub mod typescript;

use std::{fmt, future::Future, pin::Pin};

use hulladb_schema::{RecordValues, ValidationError};
use uuid::Uuid;

pub use admission::{AdmissionCleanup, AdmissionTicket};
pub use concurrency::{MutationConcurrency, RetryOptions};
pub use db::{ReadDb, Select, WriteDb};
pub use functions::{
    Authorization, FunctionContract, Invocation, KeyGenerator, Mutation, MutationContext, Query,
    QueryContext, Registry,
};
pub use operations::{OutboxStatus, ReceiptStatus, Roles, ScopeQuota, ScopeStatus, Usage};
pub use outbox::{
    ClaimOptions, DeliveryError, DeliveryFuture, DeliveryOutcome, EnqueueOptions, OutboxEntry,
    OutboxHandler, OutboxJob, OutboxWorker, WorkerOptions,
};
pub use pagination::{PageBudget, PageCursor, PageQuery, PageRange, RangePage};
pub use query_round::{QueryRound, RoundLimits, RoundQuery, RoundResult};
pub use storage::ManagedSchema;
pub use store::ManagedStore;

pub type FunctionFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send + 'a>>;

/// Supplied by trusted server integration, never decoded from function arguments.
/// Scope partitions managed data. Principal owns receipts; authorization_scope
/// must change when the verified authentication context changes. Database policy
/// changes do not retroactively revoke delivery of an already committed response.
#[derive(Debug, Clone)]
pub struct Context<I> {
    pub scope: Uuid,
    pub principal: Uuid,
    pub authorization_scope: Uuid,
    pub identity: I,
}

pub use hulladb_protocol::functions::{PageMetadata, Receipt, Revision, Snapshot};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub values: RecordValues,
    /// Storage concurrency version, separate from user-defined row fields.
    pub version: i64,
}

#[derive(Debug)]
pub enum Error {
    Client(hulladb_protocol::functions::ClientFailure),
    Forbidden,
    InvalidTicket,
    TerminalFailure(String),
    Application(hulladb_protocol::ApplicationError),
    NotFound,
    Conflict,
    ReferenceViolation,
    RowExpectationFailed,
    RequestIdReused,
    /// The request committed, but its stored result was explicitly compacted. Never replay.
    ReceiptExpired,
    InvalidInput(String),
    Validation(ValidationError),
    SchemaMismatch,
    SourceMismatch,
    QuotaExceeded,
    UnknownFunction(String),
    WrongFunctionKind,
    MissingGenerator(String),
    CorruptData,
    Database(tokio_postgres::Error),
    /// TCP, TLS, authentication and session setup share a bounded deadline.
    ConnectionDeadline,
    /// Recover using the original principal, scope, and request ID.
    CommitOutcomeUnknown,
    /// All attempts were rejected by PostgreSQL before committing.
    RetryExhausted,
    /// The managed transaction was cancelled before sending COMMIT.
    TransactionDeadline,
    /// A trusted callback worker failed before the transaction could commit.
    FunctionExecution,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Client(error) => error.fmt(f),
            Self::InvalidTicket => f.write_str("admission ticket is invalid or expired"),
            Self::Application(error) => write!(f, "application failure: {}", error.code),
            Self::TerminalFailure(code) => write!(f, "request failed: {code}"),
            Self::Forbidden => f.write_str("operation is not authorized"),
            Self::NotFound => f.write_str("row or managed scope does not exist"),
            Self::Conflict => {
                f.write_str("row identity, uniqueness, or expected version conflicts")
            }
            Self::RowExpectationFailed => f.write_str("affected row expectation failed"),
            Self::ReferenceViolation => f.write_str("row reference constraint failed"),
            Self::RequestIdReused => f.write_str("request ID was reused with different arguments"),
            Self::ReceiptExpired => {
                f.write_str("committed request result expired; reconcile without replaying")
            }
            Self::InvalidInput(reason) => f.write_str(reason),
            Self::Validation(error) => error.fmt(f),
            Self::SchemaMismatch => {
                f.write_str("installed schema differs; recreate development fixtures explicitly")
            }
            Self::SourceMismatch => {
                f.write_str("managed source changed; retire the existing execution lifecycle")
            }
            Self::QuotaExceeded => f.write_str("managed scope resource quota exceeded"),
            Self::UnknownFunction(name) => write!(f, "function {name:?} is not registered"),
            Self::WrongFunctionKind => {
                f.write_str("function execution kind does not match the request")
            }
            Self::MissingGenerator(name) => write!(f, "key generator {name:?} is not registered"),
            Self::CorruptData => f.write_str("stored managed data violates its contract"),
            Self::Database(error) => write!(f, "PostgreSQL operation failed: {error}"),
            Self::ConnectionDeadline => f.write_str("PostgreSQL connection deadline elapsed"),
            Self::CommitOutcomeUnknown => {
                f.write_str("commit outcome unknown; recover the existing request")
            }
            Self::RetryExhausted => f.write_str("serializable transaction retry budget exhausted"),
            Self::TransactionDeadline => {
                f.write_str("managed transaction deadline exceeded before commit")
            }
            Self::FunctionExecution => f.write_str("backend function worker failed"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::Validation(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ValidationError> for Error {
    fn from(value: ValidationError) -> Self {
        Self::Validation(value)
    }
}

impl From<tokio_postgres::Error> for Error {
    fn from(value: tokio_postgres::Error) -> Self {
        use tokio_postgres::error::SqlState;
        match value.code() {
            Some(&SqlState::UNIQUE_VIOLATION) => Self::Conflict,
            Some(&SqlState::FOREIGN_KEY_VIOLATION | &SqlState::RESTRICT_VIOLATION) => {
                Self::ReferenceViolation
            }
            Some(&SqlState::CHECK_VIOLATION) => {
                Self::InvalidInput("database check constraint failed".into())
            }
            _ => Self::Database(value),
        }
    }
}
