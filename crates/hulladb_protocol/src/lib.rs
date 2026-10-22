//! Portable named-function contracts and public transport errors.
use serde::{Deserialize, Serialize};

pub mod functions;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiErrorCode {
    Unauthorized,
    Forbidden,
    InvalidRequest,
    NotFound,
    Conflict,
    RequestIdReused,
    /// Committed request whose result was compacted; never resubmit as new work.
    ReceiptExpired,
    TicketExpired,
    ExecutionFailed,
    ExecutionLimit,
    QuotaExceeded,
    Capacity,
    Unavailable,
    ResetRequired,
    /// Recover or retry with the original request ID; never assume rollback.
    OutcomeUnknown,
}

/// Public errors carry no SQL, credentials, or database diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiError {
    pub code: ApiErrorCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application: Option<ApplicationError>,
}

/// Deliberately caller-visible information, never inferred from driver errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "present_details")]
    pub details: Option<serde_json::Value>,
}
pub const MAX_APPLICATION_ERROR_BYTES: usize = 16 * 1024;
impl ApplicationError {
    pub fn valid(&self) -> bool {
        !self.code.is_empty()
            && serde_json::to_vec(self)
                .is_ok_and(|bytes| bytes.len() <= MAX_APPLICATION_ERROR_BYTES)
    }
}
impl ApiError {
    pub fn valid(&self) -> bool {
        self.application
            .as_ref()
            .is_none_or(|error| self.code == ApiErrorCode::ExecutionFailed && error.valid())
    }
}

fn present_details<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<serde_json::Value>, D::Error> {
    serde_json::Value::deserialize(deserializer).map(Some)
}
