use crate::{ClientFailure, Codec, Error, FunctionFuture, Receipt, Revision, Snapshot};
use serde_json::Value as Json;
use std::{future::Future, sync::Arc, time::Duration};
use tokio::{sync::watch, task::JoinHandle};
use uuid::Uuid;

#[cfg(feature = "server")]
mod local;
#[cfg(feature = "server")]
pub use local::LocalRuntime;

pub trait Runtime: Send + Sync + 'static {
    fn query<'a>(&'a self, operation: &'a str, input: Json) -> FunctionFuture<'a, Snapshot>;
    fn mutation<'a>(&'a self, operation: &'a str, input: Json) -> FunctionFuture<'a, Receipt>;
    fn retirement(&self) -> watch::Receiver<bool>;
    fn poll_interval(&self) -> Duration;
    fn query_checked<'a>(
        &'a self,
        operation: &'a str,
        input: Json,
        validate: Validator,
    ) -> FunctionFuture<'a, Snapshot> {
        Box::pin(async move {
            let snapshot = self.query(operation, input).await?;
            validate(&snapshot.result)?;
            Ok(snapshot)
        })
    }
    fn mutation_checked<'a>(
        &'a self,
        operation: &'a str,
        input: Json,
        validate: Validator,
    ) -> FunctionFuture<'a, Receipt> {
        Box::pin(async move {
            let receipt = self.mutation(operation, input).await?;
            validate(&receipt.result)?;
            Ok(receipt)
        })
    }
    fn subscribe_checked(
        self: Arc<Self>,
        operation: String,
        input: Json,
        validate: Validator,
    ) -> FunctionFuture<'static, RawSubscription> {
        Box::pin(async move { poll_subscription(self, operation, input, validate) })
    }
    /// Remote runtimes override this with SSE; local runtimes use bounded polling.
    fn subscribe(
        self: Arc<Self>,
        operation: String,
        input: Json,
    ) -> FunctionFuture<'static, RawSubscription> {
        self.subscribe_checked(operation, input, |_| Ok(()))
    }
}
tokio::task_local! { static ACTIVE: Arc<dyn Runtime>; }
tokio::task_local! { static IN_HANDLER: (); }
pub async fn handler_scope<F: Future>(work: F) -> F::Output {
    IN_HANDLER.scope((), work).await
}
pub async fn scope<F: Future>(runtime: Arc<dyn Runtime>, work: F) -> F::Output {
    ACTIVE.scope(runtime, work).await
}
fn current() -> Result<Arc<dyn Runtime>, Error> {
    if IN_HANDLER.try_with(|_| ()).is_ok() {
        return Err(Error::InvalidInput("registered calls cannot nest inside a managed handler; compose using its context database".into()));
    }
    ACTIVE.try_with(Arc::clone).map_err(|_| {
        Error::InvalidInput("no Hulla DB application/request execution scope is active".into())
    })
}
pub type Validator = fn(&Json) -> Result<(), Error>;
pub fn validate<T: Codec>(value: &Json) -> Result<(), Error> {
    T::decode(value).map(|_| ())
}
/// Current input/output codecs required before replaying a durable mutation.
pub struct MutationContract {
    pub name: &'static str,
    pub input: Validator,
    pub output: Validator,
}
impl MutationContract {
    pub fn new<I: Codec, O: Codec>(name: &'static str) -> Self {
        Self {
            name,
            input: validate::<I>,
            output: validate::<O>,
        }
    }
}
pub async fn call_query<T: Codec>(operation: &'static str, input: Json) -> Result<T, Error> {
    T::decode(
        &current()?
            .query_checked(operation, input, validate::<T>)
            .await?
            .result,
    )
}
pub async fn call_mutation<T: Codec>(operation: &'static str, input: Json) -> Result<T, Error> {
    T::decode(
        &current()?
            .mutation_checked(operation, input, validate::<T>)
            .await?
            .result,
    )
}

#[derive(Debug, Clone)]
pub enum RawQueryState {
    Loading,
    Ready { snapshot: Snapshot, connected: bool },
    Error(Arc<Error>),
}
pub struct RawSubscription {
    pub(crate) updates: watch::Receiver<RawQueryState>,
    pub(crate) worker: JoinHandle<()>,
}
impl RawSubscription {
    pub fn new(updates: watch::Receiver<RawQueryState>, worker: JoinHandle<()>) -> Self {
        Self { updates, worker }
    }
}
impl Drop for RawSubscription {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
#[derive(Debug)]
pub enum QueryState<T> {
    Loading,
    Ready {
        data: T,
        source: Uuid,
        revision: Revision,
        connected: bool,
    },
    Error(Arc<Error>),
}
pub struct Subscription<T> {
    state: watch::Receiver<QueryState<T>>,
    worker: JoinHandle<()>,
    retired: watch::Receiver<bool>,
}
impl<T> Subscription<T> {
    pub fn state(&self) -> watch::Ref<'_, QueryState<T>> {
        self.state.borrow()
    }
    pub async fn changed(&mut self) -> Result<(), Error> {
        self.state.changed().await.map_err(|_| Error::Forbidden)
    }
    /// A connected snapshot at/after the receipt revision proves query visibility.
    pub async fn wait_visible(
        &mut self,
        receipt: &Receipt,
        budget: Duration,
    ) -> Result<(), Arc<Error>> {
        tokio::time::timeout(budget, async {
            loop {
                if *self.retired.borrow() {
                    return Err(Arc::new(Error::Forbidden));
                }
                match &*self.state.borrow() {
                    QueryState::Ready {
                        source,
                        revision,
                        connected,
                        ..
                    } => {
                        if *source != receipt.source || revision.scope != receipt.revision.scope {
                            return Err(Arc::new(Error::SourceMismatch));
                        }
                        if *connected && revision.sequence >= receipt.revision.sequence {
                            return Ok(());
                        }
                    }
                    QueryState::Error(error) => return Err(error.clone()),
                    QueryState::Loading => {}
                }
                self.state
                    .changed()
                    .await
                    .map_err(|_| Arc::new(Error::Client(ClientFailure::Closed)))?;
            }
        })
        .await
        .map_err(|_| Arc::new(Error::Client(ClientFailure::Timeout)))?
    }
}
impl<T> Drop for Subscription<T> {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
pub async fn watch_query<T: Codec>(
    operation: &'static str,
    input: Json,
) -> Result<Subscription<T>, Error> {
    let runtime = current()?;
    let mut retired = runtime.retirement();
    if *retired.borrow() {
        return Err(Error::Forbidden);
    }
    let mut raw = runtime
        .subscribe_checked(operation.into(), input, validate::<T>)
        .await?;
    let (sender, state) = watch::channel(QueryState::Loading);
    let retirement = retired.clone();
    let worker = tokio::spawn(async move {
        loop {
            let update = raw.updates.borrow_and_update().clone();
            if *retired.borrow() {
                sender.send_replace(QueryState::Error(Arc::new(Error::Forbidden)));
                return;
            }
            match update {
                RawQueryState::Loading => {
                    sender.send_replace(QueryState::Loading);
                }
                RawQueryState::Ready {
                    snapshot,
                    connected,
                } => match T::decode(&snapshot.result) {
                    Ok(data) => {
                        sender.send_replace(QueryState::Ready {
                            data,
                            source: snapshot.source,
                            revision: snapshot.revision,
                            connected,
                        });
                    }
                    Err(error) => {
                        sender.send_replace(QueryState::Error(Arc::new(error)));
                        return;
                    }
                },
                RawQueryState::Error(error) => {
                    sender.send_replace(QueryState::Error(error));
                }
            }
            tokio::select! { biased;
                _ = retired.changed() => { sender.send_replace(QueryState::Error(Arc::new(Error::Forbidden))); return; }
                result = raw.updates.changed() => if result.is_err() { return; }
            }
        }
    });
    Ok(Subscription {
        state,
        worker,
        retired: retirement,
    })
}
fn poll_subscription<R: Runtime + ?Sized>(
    runtime: Arc<R>,
    operation: String,
    input: Json,
    validate: Validator,
) -> Result<RawSubscription, Error> {
    let interval = runtime.poll_interval();
    if interval.is_zero() {
        return Err(Error::InvalidInput(
            "watch polling interval must be positive".into(),
        ));
    }
    let (sender, updates) = watch::channel(RawQueryState::Loading);
    let worker = tokio::spawn(async move {
        let mut retired = runtime.retirement();
        let mut previous = None;
        let mut ticks = tokio::time::interval(interval);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! { biased;
                _ = retired.changed() => { sender.send_replace(RawQueryState::Error(Arc::new(Error::Forbidden))); return; }
                _ = ticks.tick() => {}
            }
            let result = tokio::select! { biased;
                _ = retired.changed() => Err(Error::Forbidden),
                result = runtime.query_checked(&operation, input.clone(), validate) => result,
            };
            match result {
                Ok(snapshot) if !*retired.borrow() => {
                    if previous.as_ref() != Some(&snapshot) {
                        previous = Some(snapshot.clone());
                        sender.send_replace(RawQueryState::Ready {
                            snapshot,
                            connected: true,
                        });
                    }
                }
                Ok(_) => {
                    sender.send_replace(RawQueryState::Error(Arc::new(Error::Forbidden)));
                    return;
                }
                Err(error) => {
                    sender.send_replace(RawQueryState::Error(Arc::new(error)));
                    return;
                }
            }
        }
    });
    Ok(RawSubscription::new(updates, worker))
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "remote", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "remote", serde(deny_unknown_fields))]
pub struct MutationIntent {
    pub request_id: Uuid,
    pub operation: String,
    pub input: Json,
    pub source: Uuid,
}
