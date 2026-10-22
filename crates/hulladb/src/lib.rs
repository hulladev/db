//! Native Rust authoring for schema-driven Hulla DB functions.
//!
//! Derives emit canonical schema metadata and checked codecs. Function attributes
//! emit registry adapters and direct calls that resolve a task-local runtime.
//! See docs/rust-authoring.md for the supported subset and deployment boundary.

extern crate self as hulladb;

#[cfg(feature = "server")]
mod admin;
#[cfg(not(feature = "server"))]
mod client_error;
mod codec;
#[cfg(feature = "server")]
mod database;
#[cfg(feature = "remote")]
pub mod remote;
mod runtime;
mod table;

#[cfg(feature = "server")]
pub use admin::{AdminDatabase, Committed, TransactionOptions};
#[cfg(not(feature = "server"))]
pub use client_error::Error;
pub use codec::{Codec, Input, Scalar};
#[cfg(feature = "server")]
pub use database::{MutationContext, QueryContext, ReadDb, Select, WriteDb};
pub use hulladb_macros::{Input, Table, mutation, query};
#[cfg(feature = "server")]
pub use hulladb_postgres::managed::{
    ClaimOptions, Context, DeliveryError, DeliveryFuture, DeliveryOutcome, EnqueueOptions, Error,
    Invocation, ManagedSchema, ManagedStore, MutationConcurrency, OutboxEntry, OutboxHandler,
    OutboxJob, OutboxStatus, OutboxWorker, ReceiptStatus, Registry, RetryOptions, Roles,
    ScopeQuota, ScopeStatus, Usage, WorkerOptions,
};
pub use hulladb_protocol::functions::{ClientFailure, Receipt, Revision, Snapshot};
pub type FunctionFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, Error>> + Send + 'a>>;
pub use hulladb_schema as schema;
#[cfg(feature = "server")]
pub use runtime::LocalRuntime;
pub use runtime::{
    MutationContract, MutationIntent, QueryState, RawQueryState, RawSubscription, Runtime,
    Subscription, Validator, scope,
};
pub use table::{Field, Id, InsertInput, PatchInput, Stored, Table};
pub use uuid::Uuid;

/// A policy is mandatory even when business checks also run in the handler.
/// Confirmation delivers the original response under its verified ownership
/// fences; it does not rerun this policy or the handler.
#[cfg(feature = "server")]
pub trait Authorization<I>: Send + Sync + 'static {
    fn authorize<'a>(
        &'a self,
        context: QueryContext<'a, I>,
        invocation: Invocation,
        input: &'a serde_json::Value,
    ) -> FunctionFuture<'a, ()>;
}

#[doc(hidden)]
#[cfg(feature = "server")]
pub struct PolicyAdapter<A>(pub A);
#[cfg(feature = "server")]
impl<I: Send + Sync + 'static, A: Authorization<I>> hulladb_postgres::managed::Authorization<I>
    for PolicyAdapter<A>
{
    fn authorize<'a>(
        &'a self,
        context: hulladb_postgres::managed::QueryContext<'a, I>,
        invocation: Invocation,
        input: &'a serde_json::Value,
    ) -> FunctionFuture<'a, ()> {
        Box::pin(runtime::handler_scope(self.0.authorize(
            context.into(),
            invocation,
            input,
        )))
    }
}

#[doc(hidden)]
pub mod __private {
    pub use crate::codec::{object, reject_unknown, required};
    pub use crate::runtime::{call_mutation, call_query, handler_scope, watch_query};
    #[cfg(feature = "server")]
    pub use hulladb_postgres::managed;
    pub use serde_json::{Map, Value as Json};
}

/// Assemble canonical metadata from Rust-authored tables, including references.
#[macro_export]
macro_rules! schema {
    ($($table:ty),+ $(,)?) => {{
        let mut declaration = $crate::schema::SchemaDeclaration::new(vec![$(<$table as $crate::Table>::declaration()),+]);
        $(declaration.references.extend(<$table as $crate::Table>::references());)+
        declaration.normalize()
    }};
}
