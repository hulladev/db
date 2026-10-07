use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use hulladb_postgres::{Error, Store};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(crate) struct StorePool {
    config: tokio_postgres::Config,
    idle: Mutex<Vec<Store>>,
    slots: Arc<Semaphore>,
    closed: AtomicBool,
}

pub(crate) enum PoolError {
    Closed,
    Database(Error),
}
impl From<Error> for PoolError {
    fn from(error: Error) -> Self {
        Self::Database(error)
    }
}

pub(crate) struct Lease {
    pool: Arc<StorePool>,
    store: Option<Store>,
    _permit: OwnedSemaphorePermit,
}

impl StorePool {
    pub async fn connect(
        config: tokio_postgres::Config,
        capacity: usize,
    ) -> Result<Arc<Self>, Error> {
        let mut idle = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            idle.push(Store::connect_config(config.clone()).await?);
        }
        Ok(Arc::new(Self {
            config,
            idle: Mutex::new(idle),
            slots: Arc::new(Semaphore::new(capacity)),
            closed: AtomicBool::new(false),
        }))
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.slots.close();
        self.idle.lock().expect("pool lock poisoned").clear();
    }

    pub async fn get(self: &Arc<Self>) -> Result<Lease, PoolError> {
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| PoolError::Closed)?;
        let stored = self.idle.lock().expect("pool lock poisoned").pop();
        let store = match stored {
            Some(store) if !store.is_closed() => store,
            _ => Store::connect_config(self.config.clone()).await?,
        };
        if self.closed.load(Ordering::Acquire) {
            return Err(PoolError::Closed);
        }
        Ok(Lease {
            pool: self.clone(),
            store: Some(store),
            _permit: permit,
        })
    }
}

impl std::ops::Deref for Lease {
    type Target = Store;
    fn deref(&self) -> &Store {
        self.store.as_ref().expect("live lease")
    }
}
impl std::ops::DerefMut for Lease {
    fn deref_mut(&mut self) -> &mut Store {
        self.store.as_mut().expect("live lease")
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(store) = self.store.take().filter(|store| !store.is_closed()) {
            let mut idle = self.pool.idle.lock().expect("pool lock poisoned");
            if !self.pool.closed.load(Ordering::Acquire) {
                idle.push(store);
            }
        }
    }
}
