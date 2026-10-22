use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Process-local aggregate diagnostics. Reading never performs SQL. Counters are
/// approximate under concurrency, reset on restart, and have no identity labels.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct HostStats {
    pub requests_started: u64,
    pub requests_completed: u64,
    pub requests_cancelled: u64,
    /// Total time through response headers, including authentication/admission.
    /// Streaming body lifetime is excluded.
    pub request_duration_us: u64,
    pub successful_responses: u64,
    pub client_error_responses: u64,
    pub server_error_responses: u64,
    pub capacity_responses: u64,
    pub operation_deadlines: u64,
    pub unknown_outcomes: u64,
    pub expired_receipts: u64,
    /// Failed/timed-out bounded cleanup batches; a later tick retries safely.
    pub admission_cleanup_failures: u64,
    pub subscription_refreshes: u64,
    pub coalesced_refreshes: u64,
    pub unchanged_refreshes: u64,
    pub snapshots_delivered: u64,
    pub snapshot_bytes: u64,
    pub stream_failures: u64,
    pub active_requests: usize,
    pub active_subscriptions: usize,
    pub subscription_groups: usize,
    pub database_in_use: usize,
    pub database_waiters: usize,
    pub database_reconnects: u64,
    pub stopped: bool,
}
#[derive(Default)]
pub(super) struct Metrics {
    pub requests_started: AtomicU64,
    pub requests_completed: AtomicU64,
    pub requests_cancelled: AtomicU64,
    pub request_duration_us: AtomicU64,
    pub successful_responses: AtomicU64,
    pub client_error_responses: AtomicU64,
    pub server_error_responses: AtomicU64,
    pub capacity_responses: AtomicU64,
    pub operation_deadlines: AtomicU64,
    pub unknown_outcomes: AtomicU64,
    pub expired_receipts: AtomicU64,
    pub admission_cleanup_failures: AtomicU64,
    pub subscription_refreshes: AtomicU64,
    pub coalesced_refreshes: AtomicU64,
    pub unchanged_refreshes: AtomicU64,
    pub snapshots_delivered: AtomicU64,
    pub snapshot_bytes: AtomicU64,
    pub stream_failures: AtomicU64,
}
pub(super) fn increment(counter: &AtomicU64, value: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
        Some(n.saturating_add(value))
    });
}
pub(super) struct Request<'a> {
    metrics: &'a Metrics,
    start: Instant,
    completed: bool,
}
impl Metrics {
    pub fn request(&self) -> Request<'_> {
        increment(&self.requests_started, 1);
        Request {
            metrics: self,
            start: Instant::now(),
            completed: false,
        }
    }
}
impl Request<'_> {
    pub fn complete(&mut self, status: axum::http::StatusCode, code: Option<&str>) {
        self.completed = true;
        increment(&self.metrics.requests_completed, 1);
        if status.is_success() {
            increment(&self.metrics.successful_responses, 1);
        } else if status.is_client_error() {
            increment(&self.metrics.client_error_responses, 1);
        } else if status.is_server_error() {
            increment(&self.metrics.server_error_responses, 1);
        }
        match code {
            Some("capacity") => increment(&self.metrics.capacity_responses, 1),
            Some("outcome_unknown") => increment(&self.metrics.unknown_outcomes, 1),
            Some("receipt_expired") => increment(&self.metrics.expired_receipts, 1),
            _ => {}
        }
    }
}
impl Drop for Request<'_> {
    fn drop(&mut self) {
        increment(
            &self.metrics.request_duration_us,
            self.start.elapsed().as_micros().min(u64::MAX as u128) as u64,
        );
        if !self.completed {
            increment(&self.metrics.requests_cancelled, 1);
        }
    }
}
