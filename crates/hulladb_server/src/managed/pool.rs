use hulladb_postgres::managed::{Error, ManagedSchema, ManagedStore};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(super) struct Pool {
    url: String,
    schema: ManagedSchema,
    idle: Mutex<Vec<ManagedStore>>,
    slots: Arc<Semaphore>,
    capacity: usize,
    waiting: AtomicUsize,
    reconnects: AtomicU64,
}
pub(super) enum PoolError {
    Closed,
    Budget,
    Database(Error),
}
impl From<Error> for PoolError {
    fn from(error: Error) -> Self {
        Self::Database(error)
    }
}

pub(super) struct Lease {
    pool: Arc<Pool>,
    store: Option<ManagedStore>,
    reusable: bool,
    _permit: OwnedSemaphorePermit,
}
impl Pool {
    pub async fn connect(
        url: &str,
        schema: ManagedSchema,
        capacity: usize,
    ) -> Result<Arc<Self>, Error> {
        let mut idle = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            let mut store = ManagedStore::connect(url, schema.clone()).await?;
            store.verify().await?;
            idle.push(store);
        }
        Ok(Arc::new(Self {
            url: url.into(),
            schema,
            idle: Mutex::new(idle),
            slots: Arc::new(Semaphore::new(capacity)),
            capacity,
            waiting: AtomicUsize::new(0),
            reconnects: AtomicU64::new(0),
        }))
    }
    pub async fn get(self: &Arc<Self>) -> Result<Lease, PoolError> {
        self.waiting.fetch_add(1, Ordering::Relaxed);
        let waiting = Waiting(&self.waiting);
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| PoolError::Closed)?;
        drop(waiting);
        let stored = self.idle.lock().expect("pool lock poisoned").pop();
        let store = match stored {
            Some(store) if !store.is_closed() => store,
            _ => {
                super::metrics::increment(&self.reconnects, 1);
                let mut store = ManagedStore::connect(&self.url, self.schema.clone()).await?;
                store.verify().await?;
                store
            }
        };
        Ok(Lease {
            pool: self.clone(),
            store: Some(store),
            reusable: true,
            _permit: permit,
        })
    }
    pub fn in_use(&self) -> usize {
        self.capacity - self.slots.available_permits()
    }
    pub fn waiters(&self) -> usize {
        self.waiting.load(Ordering::Relaxed)
    }
    pub fn reconnects(&self) -> u64 {
        self.reconnects.load(Ordering::Relaxed)
    }
    pub fn close(&self) {
        self.slots.close();
        self.idle.lock().expect("pool lock poisoned").clear();
    }
}
impl Lease {
    pub async fn verify(&mut self) -> Result<(), Error> {
        // Verification clears the store's initialized state before awaiting SQL.
        // Cancellation/failure must discard it rather than poison an idle slot.
        self.reusable = false;
        let result = self.store.as_mut().expect("live lease").verify().await;
        self.reusable = result.is_ok();
        result
    }
}
impl std::ops::Deref for Lease {
    type Target = ManagedStore;
    fn deref(&self) -> &Self::Target {
        self.store.as_ref().expect("live lease")
    }
}
impl std::ops::DerefMut for Lease {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.store.as_mut().expect("live lease")
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(store) = self
            .store
            .take()
            .filter(|s| self.reusable && !s.is_closed())
        {
            let mut idle = self.pool.idle.lock().expect("pool lock poisoned");
            if !self.pool.slots.is_closed() {
                idle.push(store);
            }
        }
    }
}

struct Waiting<'a>(&'a AtomicUsize);
impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}
