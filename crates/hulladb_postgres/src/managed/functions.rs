use std::{collections::BTreeMap, sync::Arc};

use hulladb_schema::Value;
use serde_json::Value as Json;

use super::{Context, Error, FunctionFuture, MutationConcurrency, ReadDb, WriteDb};

/// Runtime decoding and result validation belong to each registered contract.
/// Generated adapters can implement these methods without exposing handlers.
pub trait FunctionContract: Send + Sync + 'static {
    /// Trusted per-invocation fetched row/byte budgets, applied before policy reads.
    #[doc(hidden)]
    fn read_limits(&self) -> Option<(usize, usize)> {
        None
    }
    fn validate_input(&self, input: &Json) -> Result<(), Error>;
    fn validate_result(&self, result: &Json) -> Result<(), Error>;
}

pub struct QueryContext<'a, I> {
    pub db: ReadDb<'a>,
    pub identity: &'a I,
    pub(super) page: Option<&'a super::pagination::PageExecution>,
}

pub struct MutationContext<'a, I> {
    pub db: WriteDb<'a, I>,
    pub identity: &'a I,
}

pub trait Query<I>: FunctionContract {
    /// Watched results may be reused until the scope revision changes. Derive
    /// them from managed reads and fixed identity/input, without clocks,
    /// randomness, external data or side effects. Direct calls always execute.
    fn call<'a>(
        &'a self,
        context: QueryContext<'a, I>,
        input: &'a Json,
    ) -> FunctionFuture<'a, Json>;
}

pub trait Mutation<I>: FunctionContract {
    /// Called within a managed transaction. Default conflict retries may replay the entire
    /// handler after a confirmed rollback. Do not perform external side
    /// effects here: a failed transaction has no receipt and may be resubmitted.
    fn call<'a>(
        &'a self,
        context: MutationContext<'a, I>,
        input: &'a Json,
    ) -> FunctionFuture<'a, Json>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invocation {
    Query,
    Mutation,
}

pub trait Authorization<I>: Send + Sync + 'static {
    /// Checked for query evaluation and each new mutation execution attempt.
    /// Reads use the same snapshot/transaction and are tracked dependencies.
    /// Confirmation delivers the original authorized response without policy replay.
    fn authorize<'a>(
        &'a self,
        context: QueryContext<'a, I>,
        invocation: Invocation,
        input: &'a Json,
    ) -> FunctionFuture<'a, ()>;
}

pub trait KeyGenerator<I>: Send + Sync + 'static {
    /// Runs in the mutation transaction. A generator may update a counter via
    /// managed primitives; its output is validated against the table's key type.
    fn generate<'a>(
        &'a self,
        context: MutationContext<'a, I>,
        table: &'a str,
    ) -> FunctionFuture<'a, Value>;
}

pub(super) enum Handler<I> {
    Query(Arc<dyn Query<I>>),
    Mutation(Arc<dyn Mutation<I>>),
}

pub(super) struct Function<I> {
    pub handler: Handler<I>,
    pub authorization: Arc<dyn Authorization<I>>,
    pub concurrency: MutationConcurrency,
}

impl<I> Function<I> {
    pub fn contract(&self) -> &dyn FunctionContract {
        match &self.handler {
            Handler::Query(handler) => handler.as_ref(),
            Handler::Mutation(handler) => handler.as_ref(),
        }
    }
}

/// Trusted registration only. Registration rejects duplicate identities rather
/// than silently replacing an operation or changing its execution kind.
pub struct Registry<I> {
    functions: BTreeMap<String, Function<I>>,
    pub(super) generators: BTreeMap<String, Arc<dyn KeyGenerator<I>>>,
}

impl<I> Default for Registry<I> {
    fn default() -> Self {
        Self {
            functions: BTreeMap::new(),
            generators: BTreeMap::new(),
        }
    }
}

impl<I: 'static> Registry<I> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn query(
        &mut self,
        name: impl Into<String>,
        handler: impl Query<I>,
        authorization: impl Authorization<I>,
    ) -> Result<(), Error> {
        self.register(
            name.into(),
            Handler::Query(Arc::new(handler)),
            Arc::new(authorization),
        )
    }

    pub fn mutation(
        &mut self,
        name: impl Into<String>,
        handler: impl Mutation<I>,
        authorization: impl Authorization<I>,
    ) -> Result<(), Error> {
        self.mutation_with_concurrency(name, handler, authorization, MutationConcurrency::default())
    }

    /// Retry registration is a declaration that the handler, authorization and
    /// generators can safely rerun. Business conflicts are never retried.
    pub fn mutation_with_concurrency(
        &mut self,
        name: impl Into<String>,
        handler: impl Mutation<I>,
        authorization: impl Authorization<I>,
        concurrency: MutationConcurrency,
    ) -> Result<(), Error> {
        concurrency.validate()?;
        let name = name.into();
        self.register(
            name.clone(),
            Handler::Mutation(Arc::new(handler)),
            Arc::new(authorization),
        )?;
        self.functions
            .get_mut(&name)
            .expect("registered function")
            .concurrency = concurrency;
        Ok(())
    }

    pub fn key_generator(
        &mut self,
        name: impl Into<String>,
        generator: impl KeyGenerator<I>,
    ) -> Result<(), Error> {
        let name = name.into();
        if name.is_empty() || self.generators.contains_key(&name) {
            return Err(Error::InvalidInput(
                "empty or duplicate generator identity".into(),
            ));
        }
        self.generators.insert(name, Arc::new(generator));
        Ok(())
    }

    fn register(
        &mut self,
        name: String,
        handler: Handler<I>,
        authorization: Arc<dyn Authorization<I>>,
    ) -> Result<(), Error> {
        if name.is_empty() || self.functions.contains_key(&name) {
            return Err(Error::InvalidInput(
                "empty or duplicate function identity".into(),
            ));
        }
        self.functions.insert(
            name,
            Function {
                handler,
                authorization,
                concurrency: MutationConcurrency::Scope,
            },
        );
        Ok(())
    }

    pub(super) fn function(&self, name: &str) -> Result<&Function<I>, Error> {
        self.functions
            .get(name)
            .ok_or_else(|| Error::UnknownFunction(name.into()))
    }
}

impl<'a, I> QueryContext<'a, I> {
    pub(super) fn new(db: ReadDb<'a>, context: &'a Context<I>) -> Self {
        Self {
            db,
            identity: &context.identity,
            page: None,
        }
    }
}

impl<'a, I> QueryContext<'a, I> {
    pub(super) fn with_page(mut self, page: &'a super::pagination::PageExecution) -> Self {
        self.page = Some(page);
        self
    }
}
