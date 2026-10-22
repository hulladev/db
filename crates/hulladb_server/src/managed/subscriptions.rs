use super::{HostState, pool::PoolError};
use hulladb_postgres::managed::{Context, RoundLimits, RoundQuery};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};
use tokio::{sync::Mutex as AsyncMutex, time::Instant};
use uuid::Uuid;

#[derive(Hash, PartialEq, Eq)]
struct Key {
    scope: Uuid,
    principal: Uuid,
    authorization_scope: Uuid,
    source: Uuid,
    queries: String,
}

/// Weak entries never keep a disconnected query or its results alive. Admission
/// is bounded by the existing stream permits before a group can be acquired.
pub(super) struct Subscriptions<I>(Mutex<HashMap<Key, Weak<Group<I>>>>);
impl<I> Default for Subscriptions<I> {
    fn default() -> Self {
        Self(Mutex::new(HashMap::new()))
    }
}
impl<I> Subscriptions<I> {
    pub fn len(&self) -> usize {
        self.0
            .lock()
            .expect("subscription map lock poisoned")
            .values()
            .filter(|group| group.strong_count() > 0)
            .count()
    }
    pub fn acquire(
        &self,
        context: &Context<I>,
        source: Uuid,
        queries: &[RoundQuery],
    ) -> Arc<Group<I>> {
        let key = Key {
            scope: context.scope,
            principal: context.principal,
            authorization_scope: context.authorization_scope,
            source,
            queries: serde_json::to_string(
                &queries
                    .iter()
                    .map(|q| (&q.key, &q.operation, &q.input, &q.page))
                    .collect::<Vec<_>>(),
            )
            .expect("JSON query inputs"),
        };
        let mut groups = self.0.lock().expect("subscription map lock poisoned");
        groups.retain(|_, group| group.strong_count() > 0);
        if let Some(group) = groups.get(&key).and_then(Weak::upgrade) {
            return group;
        }
        let group = Arc::new(Group(AsyncMutex::new(Current {
            snapshot: None,
            _identity: std::marker::PhantomData,
            next_poll: Instant::now(),
        })));
        groups.insert(key, Arc::downgrade(&group));
        group
    }
}

pub(super) struct Group<I>(AsyncMutex<Current<I>>);
struct Current<I> {
    snapshot: Option<Arc<Value>>,
    _identity: std::marker::PhantomData<I>,
    next_poll: Instant,
}
impl<I: Clone + Send + Sync + 'static> Group<I> {
    /// Driven by consumers, so no background job survives the last stream or
    /// runs ahead of backpressure. Concurrent refreshes coalesce under this lock.
    pub async fn refresh(
        &self,
        state: &HostState<I>,
        context: &Context<I>,
        source: Uuid,
        queries: &[RoundQuery],
        joining: bool,
    ) -> Result<(Arc<Value>, Instant), PoolError> {
        let mut current = self.0.lock().await;
        let due = Instant::now() >= current.next_poll || current.snapshot.is_none();
        if !joining
            && !due
            && let Some(snapshot) = &current.snapshot
        {
            super::metrics::increment(&state.metrics.coalesced_refreshes, 1);
            return Ok((snapshot.clone(), current.next_poll));
        }
        // Head is only an invalidation hint: changed membership/head always runs
        // the complete captured set through query_round's single MVCC snapshot.
        // Failed members never retry on a timer without a database change.
        let mut store = state.pool.get().await?;
        if !joining && let Some(snapshot) = &current.snapshot {
            let (head_source, revision) = store.managed_head(context).await?;
            if head_source != source {
                return Err(hulladb_postgres::managed::Error::SourceMismatch.into());
            }
            if snapshot["revision"]
                .as_str()
                .and_then(|value| value.parse::<i64>().ok())
                == Some(revision.sequence)
            {
                let snapshot = snapshot.clone();
                current.next_poll = Instant::now() + state.options.budgets.poll_interval;
                super::metrics::increment(&state.metrics.unchanged_refreshes, 1);
                return Ok((snapshot, current.next_poll));
            }
        }
        // Clear the published result before awaiting. A cancelled/failed refresh
        // leaves no cached delivery path for another consumer.
        let previous = current.snapshot.take();
        super::metrics::increment(&state.metrics.subscription_refreshes, 1);
        let snapshot = store
            .query_round(
                &state.registry,
                context,
                source,
                queries,
                RoundLimits {
                    max_queries: 128,
                    max_result_bytes: (state.options.budgets.max_body_bytes
                        - (state.options.budgets.max_body_bytes / 8).min(2 * 1024 * 1024))
                    .min(64 * 1024 * 1024),
                    timeout: state
                        .options
                        .budgets
                        .operation_timeout
                        .min(std::time::Duration::from_secs(60)),
                },
            )
            .await?;
        let snapshot = super::transport::round_json(snapshot);
        if snapshot.to_string().len() > state.options.budgets.max_body_bytes {
            return Err(PoolError::Budget);
        }
        if previous
            .as_ref()
            .is_some_and(|old| old.as_ref() == &snapshot)
        {
            super::metrics::increment(&state.metrics.unchanged_refreshes, 1);
        }
        let snapshot = previous
            .filter(|previous| previous.as_ref() == &snapshot)
            .unwrap_or_else(|| Arc::new(snapshot));
        // Joining consumers must check policy immediately, without indefinitely
        // postponing the group's regular poll under connection churn.
        if due {
            current.next_poll = Instant::now() + state.options.budgets.poll_interval;
        }
        current.snapshot = Some(snapshot.clone());
        Ok((snapshot, current.next_poll))
    }
}
