//! Authenticated HTTP/SSE serving of registered managed functions.
//! Applications supply authentication and serving; schema installation is explicit.
pub mod managed;

use std::{fmt, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticationError {
    InvalidCredentials,
    Unavailable,
}

/// Explicit resource and scheduling policy; no production defaults are implied.
#[derive(Debug, Clone, Copy)]
pub struct HostOptions {
    pub poll_interval: Duration,
    pub credential_check_interval: Duration,
    pub operation_timeout: Duration,
    pub database_connections: usize,
    pub max_inflight_requests: usize,
    pub max_subscriptions: usize,
    pub max_body_bytes: usize,
}
impl HostOptions {
    fn validate(self) -> Result<(), HostError> {
        if self.poll_interval.is_zero()
            || self.credential_check_interval.is_zero()
            || self.operation_timeout.is_zero()
            || self.database_connections == 0
            || self.max_inflight_requests == 0
            || self.max_inflight_requests > tokio::sync::Semaphore::MAX_PERMITS
            || self.max_body_bytes == 0
            || !(1..=128).contains(&self.max_subscriptions)
            || self.database_connections > tokio::sync::Semaphore::MAX_PERMITS
        {
            return Err(HostError::InvalidOptions);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum HostError {
    InvalidOptions,
    Managed(hulladb_postgres::managed::Error),
}
impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOptions => f.write_str(
                "host budgets/intervals must be positive; subscription capacity is 1..128",
            ),
            Self::Managed(e) => fmt::Display::fmt(e, f),
        }
    }
}
impl std::error::Error for HostError {}
