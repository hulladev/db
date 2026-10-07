//! Authenticated HTTP and scheduled synchronization for the fixed task slice.
//! The embedding application supplies authentication and HTTP/TLS serving.
//! This host never installs or resets a database schema.
mod auth;
mod pool;
mod transport;

use std::{
    fmt,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use hulladb_postgres::Store;
use hulladb_sync::TaskFeed;
use tokio::{
    sync::{Mutex, Notify, watch},
    task::JoinHandle,
    time::{MissedTickBehavior, timeout},
};
use uuid::Uuid;

pub use auth::{AuthenticationError, AuthenticationFuture, Authenticator};
pub use hulladb_protocol::{ApiError, ApiErrorCode, CommandRequest};
use pool::StorePool;

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
    Database(hulladb_postgres::Error),
    Feed(hulladb_sync::TaskFeedError),
}
impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOptions => f.write_str(
                "host budgets/intervals must be positive; task subscription capacity is 1..128",
            ),
            Self::Database(e) => fmt::Display::fmt(e, f),
            Self::Feed(e) => fmt::Display::fmt(e, f),
        }
    }
}
impl std::error::Error for HostError {}
impl From<hulladb_postgres::Error> for HostError {
    fn from(e: hulladb_postgres::Error) -> Self {
        Self::Database(e)
    }
}
impl From<hulladb_sync::TaskFeedError> for HostError {
    fn from(e: hulladb_sync::TaskFeedError) -> Self {
        Self::Feed(e)
    }
}

struct Coordinator {
    store: Store,
    feed: TaskFeed,
}
struct HostState {
    auth: Arc<dyn Authenticator>,
    options: HostOptions,
    config: tokio_postgres::Config,
    pool: Arc<StorePool>,
    requests: Arc<tokio::sync::Semaphore>,
    streams: Arc<tokio::sync::Semaphore>,
    coordinator: Mutex<Option<Coordinator>>,
    ready: AtomicBool,
    stopping: AtomicBool,
    epoch: watch::Sender<Uuid>,
    wake: Notify,
}
impl HostState {
    fn available(&self) -> bool {
        self.ready.load(Ordering::Acquire) && !self.stopping.load(Ordering::Acquire)
    }
    fn invalidate(&self) {
        self.ready.store(false, Ordering::Release);
        self.epoch.send_replace(Uuid::now_v7());
    }
}

/// Keep this owner alive while serving its router. stop closes streams; shutdown
/// also joins the scheduler. Dropping the owner aborts its background worker.
pub struct TaskHost {
    state: Arc<HostState>,
    scheduler: JoinHandle<()>,
}
impl TaskHost {
    pub async fn connect(
        config: tokio_postgres::Config,
        auth: Arc<dyn Authenticator>,
        options: HostOptions,
    ) -> Result<Self, HostError> {
        options.validate()?;
        let store = Store::connect_config(config.clone()).await?;
        let feed = TaskFeed::new(&store, options.max_subscriptions).await?;
        let pool = StorePool::connect(config.clone(), options.database_connections).await?;
        let (epoch, _) = watch::channel(Uuid::now_v7());
        let state = Arc::new(HostState {
            auth,
            options,
            config,
            pool,
            requests: Arc::new(tokio::sync::Semaphore::new(options.max_inflight_requests)),
            streams: Arc::new(tokio::sync::Semaphore::new(options.max_subscriptions)),
            coordinator: Mutex::new(Some(Coordinator { store, feed })),
            ready: AtomicBool::new(true),
            stopping: AtomicBool::new(false),
            epoch,
            wake: Notify::new(),
        });
        let scheduler = tokio::spawn(schedule(Arc::downgrade(&state), options.poll_interval));
        Ok(Self { state, scheduler })
    }

    pub fn router(&self) -> axum::Router {
        transport::router(self.state.clone())
    }

    pub fn stop(&self) {
        if !self.state.stopping.swap(true, Ordering::AcqRel) {
            self.state.invalidate();
            self.state.pool.close();
        }
        self.state.wake.notify_one();
    }

    pub async fn shutdown(mut self) {
        self.stop();
        // Abort also cancels a blocked poll; its transaction is dropped safely.
        self.scheduler.abort();
        let _ = (&mut self.scheduler).await;
        self.state.coordinator.lock().await.take();
    }
}
impl Drop for TaskHost {
    fn drop(&mut self) {
        self.stop();
        self.scheduler.abort();
    }
}

async fn schedule(state: Weak<HostState>, cadence: Duration) {
    let mut ticks = tokio::time::interval(cadence);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        // Holding this Arc during one tick is bounded; the owner aborts on drop.
        let Some(state) = state.upgrade() else { break };
        tokio::select! { _ = ticks.tick() => {}, _ = state.wake.notified() => {} }
        if state.stopping.load(Ordering::Acquire) {
            break;
        }
        let mut coordinator = state.coordinator.lock().await;
        if let Some(active) = coordinator.as_mut() {
            let Coordinator { store, feed } = active;
            if !matches!(
                timeout(state.options.operation_timeout, feed.refresh(store)).await,
                Ok(Ok(()))
            ) {
                coordinator.take();
                state.invalidate();
            }
        } else {
            let reconnect = async {
                let store = Store::connect_config(state.config.clone()).await?;
                let feed = TaskFeed::new(&store, state.options.max_subscriptions).await?;
                Ok::<_, HostError>(Coordinator { store, feed })
            };
            if let Ok(Ok(active)) = timeout(state.options.operation_timeout, reconnect).await {
                *coordinator = Some(active);
                state.ready.store(true, Ordering::Release);
            }
        }
    }
}
