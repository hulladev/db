//! Private bridge support for ordinary application-server database callbacks.
//! These transactions have no function registration, authorization, or receipts.
use super::{Context, Error, FunctionFuture, ManagedStore, ReadDb, Registry, WriteDb};
use std::{
    collections::BTreeSet,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio_postgres::IsolationLevel;
use uuid::Uuid;

impl ManagedStore {
    /// Execute one trusted server callback, once. `false` in its result requests
    /// explicit rollback. Only a successful COMMIT returns `true`; transport
    /// failure during commit is reported as an unknown outcome, never replayed.
    #[doc(hidden)]
    pub async fn direct_transaction<T: Send, F>(
        &mut self,
        scope: Uuid,
        writable: bool,
        callback: F,
    ) -> Result<(T, bool), Error>
    where
        F: for<'a> FnOnce(ReadDb<'a>, Option<WriteDb<'a, ()>>) -> FunctionFuture<'a, (T, bool)>,
    {
        self.require_initialized()?;
        let tx = self
            .client
            .build_transaction()
            .isolation_level(if writable {
                IsolationLevel::Serializable
            } else {
                IsolationLevel::RepeatableRead
            })
            .read_only(!writable)
            .start()
            .await?;
        let result = async {
            let (source, _) = super::store::head(&tx, &self.schema, scope, false).await?;
            let reads = Mutex::new(BTreeSet::new());
            let dirty = AtomicBool::new(false);
            let registry = Registry::default();
            let context = Context {
                scope,
                principal: Uuid::nil(),
                authorization_scope: Uuid::nil(),
                identity: (),
            };
            let outbox = super::outbox::EnqueueContext::new(source, Uuid::now_v7(), "direct");
            let read_budget = Mutex::new(super::db::ReadBudget::default());
            let read = ReadDb {
                tx: &tx,
                schema: &self.schema,
                scope,
                reads: &reads,
                read_budget: &read_budget,
            };
            let write = writable.then_some(WriteDb {
                read,
                context: &context,
                registry: &registry,
                dirty: &dirty,
                outbox: &outbox,
            });
            let result = callback(read, write).await?;
            read.read_usage()?;
            if result.1 && dirty.load(Ordering::Relaxed) {
                super::store::advance_revision(&tx, &self.schema, scope).await?;
                super::operations::enforce_quota(&tx, &self.schema, scope, 0).await?;
            }
            Ok(result)
        }
        .await;
        match result {
            Ok((value, true)) => {
                tx.commit().await.map_err(super::store::commit_error)?;
                Ok((value, true))
            }
            Ok((value, false)) => {
                tx.rollback().await?;
                Ok((value, false))
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    }
}
