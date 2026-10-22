use crate::ClientFailure;

/// Client-only builds retain portable codec, authorization and recovery errors.
#[derive(Debug)]
pub enum Error {
    Forbidden,
    NotFound,
    Conflict,
    RequestIdReused,
    ReceiptExpired,
    InvalidTicket,
    TerminalFailure(String),
    Application(hulladb_protocol::ApplicationError),
    TransactionDeadline,
    InvalidInput(String),
    CorruptData,
    SourceMismatch,
    QuotaExceeded,
    CommitOutcomeUnknown,
    Client(ClientFailure),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput(message) => f.write_str(message),
            Self::Client(error) => error.fmt(f),
            other => write!(f, "Hulla DB operation failed: {other:?}"),
        }
    }
}
impl std::error::Error for Error {}
