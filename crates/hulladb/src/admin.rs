use crate::{
    Codec, Context, Error, FunctionFuture, ManagedSchema, ManagedStore, MutationConcurrency,
    MutationContext, QueryContext, Receipt, Registry, RetryOptions, Uuid,
};
use hulladb_postgres::managed;
use serde_json::Value as Json;
use std::{marker::PhantomData, time::Duration};

/// Server-only typed access. The operator connection must own schema CREATE
/// privilege; the serving runtime role cannot construct this entry point.
/// Mutations still validate constraints, versions, quotas, revision and receipt.
pub struct AdminDatabase<I> {
    store: ManagedStore,
    context: Context<I>,
    source: Uuid,
}
pub struct Committed<T> {
    pub data: T,
    pub receipt: Receipt,
}
/// Replay is opt-in for each direct transaction call. External effects must run
/// after a retried transaction completes because rollback cannot undo them.
#[derive(Debug, Clone, Copy)]
pub struct TransactionOptions {
    /// Additional attempts after the initial attempt. Defaults to zero.
    pub retries: u16,
    /// Shared budget for all attempts, lock waits, recovery and backoff.
    pub timeout: Duration,
    pub backoff: Duration,
}
impl Default for TransactionOptions {
    fn default() -> Self {
        let budget = RetryOptions::default();
        Self {
            retries: 0,
            timeout: budget.timeout,
            backoff: budget.backoff,
        }
    }
}
struct AdminPolicy;
impl<I: Send + Sync + 'static> managed::Authorization<I> for AdminPolicy {
    fn authorize<'a>(
        &'a self,
        _: managed::QueryContext<'a, I>,
        _: managed::Invocation,
        _: &'a Json,
    ) -> FunctionFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}
struct Handler<I, A, R, F> {
    work: F,
    marker: PhantomData<fn(I, A) -> R>,
}
impl<I: 'static, A: Codec, R: Codec, F: Send + Sync + 'static> managed::FunctionContract
    for Handler<I, A, R, F>
{
    fn validate_input(&self, input: &Json) -> Result<(), Error> {
        A::decode(input).map(|_| ())
    }
    fn validate_result(&self, result: &Json) -> Result<(), Error> {
        R::decode(result).map(|_| ())
    }
}
impl<I: Send + Sync + 'static, A: Codec, R: Codec, F> managed::Query<I> for Handler<I, A, R, F>
where
    F: for<'a> Fn(QueryContext<'a, I>, A) -> FunctionFuture<'a, R> + Send + Sync + 'static,
{
    fn call<'a>(
        &'a self,
        context: managed::QueryContext<'a, I>,
        input: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        Box::pin(crate::runtime::handler_scope(async move {
            (self.work)(context.into(), A::decode(input)?)
                .await?
                .encode()
        }))
    }
}
impl<I: Send + Sync + 'static, A: Codec, R: Codec, F> managed::Mutation<I> for Handler<I, A, R, F>
where
    F: for<'a> Fn(MutationContext<'a, I>, A) -> FunctionFuture<'a, R> + Send + Sync + 'static,
{
    fn call<'a>(
        &'a self,
        context: managed::MutationContext<'a, I>,
        input: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        Box::pin(crate::runtime::handler_scope(async move {
            (self.work)(context.into(), A::decode(input)?)
                .await?
                .encode()
        }))
    }
}
impl<I: Send + Sync + 'static> AdminDatabase<I> {
    pub async fn connect(
        url: &str,
        schema: ManagedSchema,
        context: Context<I>,
    ) -> Result<Self, Error> {
        let mut store = ManagedStore::connect(url, schema).await?;
        store.verify().await?;
        store.require_operator().await?;
        let (source, _) = store.managed_head(&context).await?;
        Ok(Self {
            store,
            context,
            source,
        })
    }
    pub async fn query<A: Codec, R: Codec, F>(
        &mut self,
        name: &str,
        input: A,
        work: F,
    ) -> Result<R, Error>
    where
        F: for<'a> Fn(QueryContext<'a, I>, A) -> FunctionFuture<'a, R> + Send + Sync + 'static,
    {
        let mut registry = Registry::new();
        registry.query(
            name,
            Handler {
                work,
                marker: PhantomData::<fn(I, A) -> R>,
            },
            AdminPolicy,
        )?;
        let snapshot = self
            .store
            .query(&registry, &self.context, name, input.encode()?)
            .await?;
        if snapshot.source != self.source {
            return Err(Error::SourceMismatch);
        }
        R::decode(&snapshot.result)
    }
    /// Use a stable operation name, caller-owned request UUID and explicit input
    /// for jobs. Repeating the same request recovers without rerunning work.
    /// Callback captures must not change the meaning of the explicit input.
    pub async fn transaction<A: Codec, R: Codec, F>(
        &mut self,
        name: &str,
        request: Uuid,
        input: A,
        work: F,
    ) -> Result<Committed<R>, Error>
    where
        F: for<'a> Fn(MutationContext<'a, I>, A) -> FunctionFuture<'a, R> + Send + Sync + 'static,
    {
        self.transaction_with_options(name, request, input, work, TransactionOptions::default())
            .await
    }

    /// Serializable direct transaction with explicit whole-callback retry opt-in.
    /// Only confirmed PostgreSQL concurrency failures can replay the callback.
    pub async fn transaction_with_options<A: Codec, R: Codec, F>(
        &mut self,
        name: &str,
        request: Uuid,
        input: A,
        work: F,
        options: TransactionOptions,
    ) -> Result<Committed<R>, Error>
    where
        F: for<'a> Fn(MutationContext<'a, I>, A) -> FunctionFuture<'a, R> + Send + Sync + 'static,
    {
        let max_attempts = options
            .retries
            .checked_add(1)
            .ok_or_else(|| Error::InvalidInput("transaction retries must be 0..15".into()))?;
        let mut registry = Registry::new();
        registry.mutation_with_concurrency(
            name,
            Handler {
                work,
                marker: PhantomData::<fn(I, A) -> R>,
            },
            AdminPolicy,
            MutationConcurrency::Serializable(RetryOptions {
                max_attempts,
                timeout: options.timeout,
                backoff: options.backoff,
            }),
        )?;
        let receipt = self
            .store
            .mutate_from_source(
                &registry,
                &self.context,
                request,
                Some(self.source),
                name,
                input.encode()?,
            )
            .await?;
        Ok(Committed {
            data: R::decode(&receipt.result)?,
            receipt,
        })
    }
}
