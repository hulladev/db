//! Generic HTTP/SSE serving of registered managed PostgreSQL functions.
//! Hosts verify existing storage; schema installation/provisioning is explicit.

mod metrics;
mod pool;
pub use metrics::HostStats;
mod quotas;
mod subscriptions;
mod transport;
pub use quotas::PrincipalQuotas;

use crate::{AuthenticationError, HostOptions};
use hulladb_postgres::managed::{Context, ManagedSchema, Registry};
use std::{future::Future, pin::Pin, sync::Arc};
use tokio::sync::{Semaphore, watch};

/// Original request headers supplied to the trusted application verifier.
/// Cookies, bearer tokens and custom credential schemes remain application-owned.
pub use axum::http::HeaderMap as CredentialHeaders;

pub type AuthenticationFuture<'a, I> =
    Pin<Box<dyn Future<Output = Result<Context<I>, AuthenticationError>> + Send + 'a>>;
/// Trusted application authentication integration, invoked on every request and
/// periodically/before delivery for live subscriptions using their original headers.
/// Accept missing credentials by returning an anonymous context, or reject them
/// with `InvalidCredentials`. Supplied headers (including Hulla context pins) are
/// untrusted input, never proof of identity. Verify credentials with your provider.
///
/// Return stable scope/principal/authorization-scope values. Change the
/// authorization scope whenever identity claims relevant to query results change;
/// live delivery is retired when any of those three context values changes.
/// Expiry, refresh and revocation remain the authentication provider's job.
pub trait Authenticator<I>: Send + Sync + 'static {
    fn authenticate<'a>(&'a self, headers: &'a CredentialHeaders) -> AuthenticationFuture<'a, I>;
}

#[derive(Clone)]
pub struct Options {
    pub budgets: HostOptions,
    /// Explicit browser origins; an empty list accepts requests without Origin.
    pub cors_origins: Vec<String>,
    pub principal_quotas: Option<PrincipalQuotas>,
}
pub(super) struct HostState<I> {
    auth: Arc<dyn Authenticator<I>>,
    registry: Registry<I>,
    options: Options,
    pool: Arc<pool::Pool>,
    requests: Arc<Semaphore>,
    streams: Arc<Semaphore>,
    stopped: watch::Sender<bool>,
    quotas: Option<Arc<quotas::Gate>>,
    subscriptions: subscriptions::Subscriptions<I>,
    metrics: metrics::Metrics,
}

/// Keep the owner alive while serving its router. Drop/stop retires open streams.
pub struct ManagedHost<I> {
    state: Arc<HostState<I>>,
    cleanup: tokio::task::JoinHandle<()>,
}
impl<I: Clone + Send + Sync + 'static> ManagedHost<I> {
    pub async fn connect(
        url: &str,
        schema: ManagedSchema,
        registry: Registry<I>,
        auth: Arc<dyn Authenticator<I>>,
        options: Options,
    ) -> Result<Self, crate::HostError> {
        options.budgets.validate()?;
        if options
            .principal_quotas
            .is_some_and(|quotas| !quotas.valid())
        {
            return Err(crate::HostError::InvalidOptions);
        }
        if options.cors_origins.iter().any(|origin| {
            origin == "null"
                || origin == "*"
                || !origin.contains("://")
                || origin.parse::<axum::http::HeaderValue>().is_err()
        }) {
            return Err(crate::HostError::InvalidOptions);
        }
        let pool = pool::Pool::connect(url, schema, options.budgets.database_connections)
            .await
            .map_err(crate::HostError::Managed)?;
        let (stopped, _) = watch::channel(false);
        let state = Arc::new(HostState {
            requests: Arc::new(Semaphore::new(options.budgets.max_inflight_requests)),
            streams: Arc::new(Semaphore::new(options.budgets.max_subscriptions)),
            auth,
            registry,
            quotas: options.principal_quotas.map(quotas::Gate::new),
            options,
            pool,
            stopped,
            subscriptions: subscriptions::Subscriptions::default(),
            metrics: metrics::Metrics::default(),
        });
        let weak = Arc::downgrade(&state);
        let mut stopped = state.stopped.subscribe();
        let cleanup = tokio::spawn(async move {
            let mut ticks = tokio::time::interval(std::time::Duration::from_secs(1));
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = stopped.changed() => break,
                    _ = ticks.tick() => {}
                }
                let Some(state) = weak.upgrade() else { break };
                if *stopped.borrow() {
                    break;
                }
                let cleanup = async {
                    loop {
                        // Release the pool slot between bounded batches so a
                        // backlog cannot monopolize foreground connections.
                        let batch = state
                            .pool
                            .get()
                            .await?
                            .cleanup_admissions(100)
                            .await
                            .map_err(pool::PoolError::from)?;
                        if [
                            batch.retired_tickets,
                            batch.failed_requests,
                            batch.purged_requests,
                            batch.compacted_results,
                        ]
                        .into_iter()
                        .all(|count| count < 100)
                        {
                            return Ok::<(), pool::PoolError>(());
                        }
                        tokio::task::yield_now().await;
                    }
                };
                tokio::select! {
                    _ = stopped.changed() => break,
                    result = tokio::time::timeout(state.options.budgets.operation_timeout, cleanup) => {
                        if !matches!(result, Ok(Ok(_))) {
                            metrics::increment(&state.metrics.admission_cleanup_failures, 1);
                        }
                    }
                }
            }
        });
        Ok(Self { state, cleanup })
    }
    pub fn router(&self) -> axum::Router {
        transport::router(self.state.clone())
    }
    pub async fn ready(&self) -> bool {
        if *self.state.stopped.borrow() {
            return false;
        }
        matches!(
            tokio::time::timeout(self.state.options.budgets.operation_timeout, async {
                self.state
                    .pool
                    .get()
                    .await?
                    .verify()
                    .await
                    .map_err(pool::PoolError::from)
            })
            .await,
            Ok(Ok(()))
        )
    }
}
impl<I> ManagedHost<I> {
    /// Export through an application-owned, authenticated operator surface.
    /// The public function router does not expose these diagnostics.
    pub fn stats(&self) -> HostStats {
        use std::sync::atomic::Ordering;
        HostStats {
            requests_started: self.state.metrics.requests_started.load(Ordering::Relaxed),
            requests_completed: self
                .state
                .metrics
                .requests_completed
                .load(Ordering::Relaxed),
            requests_cancelled: self
                .state
                .metrics
                .requests_cancelled
                .load(Ordering::Relaxed),
            request_duration_us: self
                .state
                .metrics
                .request_duration_us
                .load(Ordering::Relaxed),
            successful_responses: self
                .state
                .metrics
                .successful_responses
                .load(Ordering::Relaxed),
            client_error_responses: self
                .state
                .metrics
                .client_error_responses
                .load(Ordering::Relaxed),
            server_error_responses: self
                .state
                .metrics
                .server_error_responses
                .load(Ordering::Relaxed),
            capacity_responses: self
                .state
                .metrics
                .capacity_responses
                .load(Ordering::Relaxed),
            operation_deadlines: self
                .state
                .metrics
                .operation_deadlines
                .load(Ordering::Relaxed),
            unknown_outcomes: self.state.metrics.unknown_outcomes.load(Ordering::Relaxed),
            expired_receipts: self.state.metrics.expired_receipts.load(Ordering::Relaxed),
            admission_cleanup_failures: self
                .state
                .metrics
                .admission_cleanup_failures
                .load(Ordering::Relaxed),
            subscription_refreshes: self
                .state
                .metrics
                .subscription_refreshes
                .load(Ordering::Relaxed),
            coalesced_refreshes: self
                .state
                .metrics
                .coalesced_refreshes
                .load(Ordering::Relaxed),
            unchanged_refreshes: self
                .state
                .metrics
                .unchanged_refreshes
                .load(Ordering::Relaxed),
            snapshots_delivered: self
                .state
                .metrics
                .snapshots_delivered
                .load(Ordering::Relaxed),
            snapshot_bytes: self.state.metrics.snapshot_bytes.load(Ordering::Relaxed),
            stream_failures: self.state.metrics.stream_failures.load(Ordering::Relaxed),
            active_requests: self.state.options.budgets.max_inflight_requests
                - self.state.requests.available_permits(),
            active_subscriptions: self.state.options.budgets.max_subscriptions
                - self.state.streams.available_permits(),
            subscription_groups: self.state.subscriptions.len(),
            database_in_use: self.state.pool.in_use(),
            database_waiters: self.state.pool.waiters(),
            database_reconnects: self.state.pool.reconnects(),
            stopped: *self.state.stopped.borrow(),
        }
    }
    pub fn stop(&self) {
        self.state.stopped.send_replace(true);
        self.cleanup.abort();
        self.state.pool.close();
    }
}
impl<I> Drop for ManagedHost<I> {
    fn drop(&mut self) {
        self.stop();
    }
}
