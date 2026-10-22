use std::time::Duration;

use super::Error;

/// Serializable lets declared handlers overlap and relies on PostgreSQL to reject
/// serialization anomalies. Scope coordination remains an explicit override.
/// The revision row still orders final writes.
#[derive(Debug, Clone, Copy)]
pub enum MutationConcurrency {
    Scope,
    Serializable(RetryOptions),
}

/// Whole-handler conflict retry budget. Registered mutations default to four
/// attempts; handlers, policies and generators must be safe to replay.
#[derive(Debug, Clone, Copy)]
pub struct RetryOptions {
    pub max_attempts: u16,
    /// Total execution budget, including attempts, lock waits, and backoff.
    pub timeout: Duration,
    /// Exponential, jittered backoff, capped by the remaining total budget.
    pub backoff: Duration,
}

impl Default for RetryOptions {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            timeout: Duration::from_secs(10),
            backoff: Duration::from_millis(10),
        }
    }
}

impl Default for MutationConcurrency {
    fn default() -> Self {
        Self::Serializable(RetryOptions::default())
    }
}

impl MutationConcurrency {
    pub(super) fn validate(self) -> Result<(), Error> {
        if let Self::Serializable(options) = self
            && (options.max_attempts == 0
                || options.max_attempts > 16
                || options.timeout.is_zero()
                || options.timeout > Duration::from_secs(60)
                || options.backoff > options.timeout)
        {
            return Err(Error::InvalidInput("serializable attempts must be 1..16 with a positive total budget up to 60 seconds and backoff within that budget".into()));
        }
        Ok(())
    }
}

pub(super) fn retryable(error: &Error) -> bool {
    use tokio_postgres::error::SqlState;
    matches!(error, Error::Database(error) if matches!(error.code(), Some(&SqlState::T_R_SERIALIZATION_FAILURE | &SqlState::T_R_DEADLOCK_DETECTED)))
}
