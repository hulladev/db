use super::*;
use crate::{Context, ManagedSchema, ManagedStore, Registry};
use std::{collections::BTreeMap, sync::Mutex as SyncMutex};
use tokio::sync::Mutex;

/// Embedded/server runtime. Fixed verified context, checked storage, and an
/// in-memory request journal. This is not a persistent native HTTP client.
pub struct LocalRuntime<I> {
    url: String,
    schema: ManagedSchema,
    store: Mutex<ManagedStore>,
    registry: Registry<I>,
    context: Context<I>,
    source: Uuid,
    pending: SyncMutex<BTreeMap<Uuid, MutationIntent>>,
    retired: watch::Sender<bool>,
    interval: Duration,
}
impl<I: Send + Sync + 'static> LocalRuntime<I> {
    pub async fn connect(
        url: &str,
        schema: ManagedSchema,
        registry: Registry<I>,
        context: Context<I>,
        interval: Duration,
    ) -> Result<Self, Error> {
        if interval.is_zero() {
            return Err(Error::InvalidInput(
                "watch polling interval must be positive".into(),
            ));
        }
        let mut store = ManagedStore::connect(url, schema.clone()).await?;
        store.verify().await?;
        let (source, _) = store.managed_head(&context).await?;
        let (retired, _) = watch::channel(false);
        Ok(Self {
            url: url.into(),
            schema,
            store: Mutex::new(store),
            registry,
            context,
            source,
            pending: SyncMutex::new(BTreeMap::new()),
            retired,
            interval,
        })
    }
    /// Retire old calls/watches before configuring a replacement identity scope.
    pub fn retire(&self) {
        self.retired.send_replace(true);
    }
    pub fn pending(&self) -> Result<Vec<MutationIntent>, Error> {
        Ok(self
            .pending
            .lock()
            .map_err(|_| Error::CorruptData)?
            .values()
            .cloned()
            .collect())
    }
    pub async fn recover_pending(&self) -> Result<Vec<Receipt>, Error> {
        self.check()?;
        let mut recovered = Vec::new();
        for intent in self.pending()? {
            let mut store = self.store.lock().await;
            if !self
                .pending
                .lock()
                .map_err(|_| Error::CorruptData)?
                .contains_key(&intent.request_id)
            {
                continue;
            }
            self.reconnect(&mut store).await?;
            self.check()?;
            let receipt = match store
                .receipt_from_source(&self.context, intent.request_id, Some(intent.source))
                .await?
            {
                Some(receipt) => receipt,
                None => {
                    store
                        .mutate_from_source(
                            &self.registry,
                            &self.context,
                            intent.request_id,
                            Some(intent.source),
                            &intent.operation,
                            intent.input.clone(),
                        )
                        .await?
                }
            };
            // A retired acknowledgement remains recoverable by its original
            // owner; retirement never asserts that the mutation did not commit.
            if self.check().is_err() {
                return Err(Error::CommitOutcomeUnknown);
            }
            self.pending
                .lock()
                .map_err(|_| Error::CorruptData)?
                .remove(&intent.request_id);
            recovered.push(receipt);
        }
        Ok(recovered)
    }
    fn check(&self) -> Result<(), Error> {
        if *self.retired.borrow() {
            Err(Error::Forbidden)
        } else {
            Ok(())
        }
    }
    async fn reconnect(&self, store: &mut ManagedStore) -> Result<(), Error> {
        if store.is_closed() {
            let mut replacement = ManagedStore::connect(&self.url, self.schema.clone()).await?;
            replacement.verify().await?;
            *store = replacement;
        }
        Ok(())
    }
}
impl<I: Send + Sync + 'static> Runtime for LocalRuntime<I> {
    fn query<'a>(&'a self, operation: &'a str, input: Json) -> FunctionFuture<'a, Snapshot> {
        Box::pin(async move {
            self.check()?;
            let mut store = self.store.lock().await;
            self.reconnect(&mut store).await?;
            self.check()?;
            let snapshot = store
                .query(&self.registry, &self.context, operation, input)
                .await?;
            self.check()?;
            if snapshot.source != self.source {
                return Err(Error::SourceMismatch);
            }
            Ok(snapshot)
        })
    }
    fn mutation<'a>(&'a self, operation: &'a str, input: Json) -> FunctionFuture<'a, Receipt> {
        Box::pin(async move {
            self.check()?;
            let intent = MutationIntent {
                request_id: Uuid::now_v7(),
                operation: operation.into(),
                input,
                source: self.source,
            };
            self.pending
                .lock()
                .map_err(|_| Error::CorruptData)?
                .insert(intent.request_id, intent.clone());
            let mut store = self.store.lock().await;
            // Failures/cancellation before submission keep an absent request
            // recoverable with exactly the same identity and encoded input.
            self.reconnect(&mut store).await?;
            self.check()?;
            let result = store
                .mutate_from_source(
                    &self.registry,
                    &self.context,
                    intent.request_id,
                    Some(intent.source),
                    operation,
                    intent.input,
                )
                .await;
            if self.check().is_err() {
                return Err(Error::CommitOutcomeUnknown);
            }
            if !matches!(result, Err(Error::CommitOutcomeUnknown)) {
                self.pending
                    .lock()
                    .map_err(|_| Error::CorruptData)?
                    .remove(&intent.request_id);
            }
            result
        })
    }
    fn retirement(&self) -> watch::Receiver<bool> {
        self.retired.subscribe()
    }
    fn poll_interval(&self) -> Duration {
        self.interval
    }
}
