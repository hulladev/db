use std::{fmt, sync::Arc};

use hulladb_postgres::{ManagedHead, Session, Store, TaskQuery};
use hulladb_protocol::{DeliveryCursor, TaskMessage, TaskResult, VisibilityFence};
use tokio::sync::watch;

#[derive(Debug)]
pub enum TaskFeedError {
    Authorization(hulladb_postgres::Error),
    Query(hulladb_postgres::Error),
    SourceMismatch,
    Limit,
    Closed,
}
impl fmt::Display for TaskFeedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authorization(e) | Self::Query(e) => fmt::Display::fmt(e, f),
            Self::SourceMismatch => f.write_str("managed source identity or revision changed"),
            Self::Limit => f.write_str("task subscription limit exceeded"),
            Self::Closed => f.write_str("task subscription is closed"),
        }
    }
}
impl std::error::Error for TaskFeedError {}

struct Group {
    session: Session,
    query: TaskQuery,
    observed: ManagedHead,
    sender: watch::Sender<Arc<TaskMessage>>,
}

/// Server query maintenance driven by durable managed transaction revisions.
/// Only projected results are retained here; no full server row replica is needed.
/// The host MUST poll refresh periodically and after reconnect; an immediate call
/// after commit is only a latency optimization. Notifications are not checkpoints.
pub struct TaskFeed {
    source: uuid::Uuid,
    max_subscriptions: usize,
    groups: Vec<Group>,
}

pub struct TaskSubscription {
    session: Session,
    source: uuid::Uuid,
    receiver: Option<watch::Receiver<Arc<TaskMessage>>>,
}

impl TaskFeed {
    pub async fn new(store: &Store, max_subscriptions: usize) -> Result<Self, TaskFeedError> {
        if max_subscriptions == 0 || max_subscriptions > 128 {
            return Err(TaskFeedError::Limit);
        }
        let source = store.managed_source().await.map_err(TaskFeedError::Query)?;
        Ok(Self {
            source,
            max_subscriptions,
            groups: Vec::new(),
        })
    }

    pub async fn subscribe(
        &mut self,
        store: &mut Store,
        session: Session,
        query: TaskQuery,
    ) -> Result<TaskSubscription, TaskFeedError> {
        self.check_source(store).await?;
        self.groups.retain(|g| {
            g.sender.receiver_count() > 0
                && matches!(g.sender.borrow().result, TaskResult::Snapshot { .. })
        });
        if self
            .groups
            .iter()
            .map(|g| g.sender.receiver_count())
            .sum::<usize>()
            >= self.max_subscriptions
        {
            return Err(TaskFeedError::Limit);
        }
        // The revision and authorized rows share one repeatable-read snapshot.
        // A commit during client bootstrap advances a later durable head; polling
        // catches it even if no process received a wake-up or the service restarted.
        let result = store
            .query_snapshot(session, query)
            .await
            .map_err(query_error)?;
        let observed = ManagedHead {
            source: result.source,
            revision: result.revision,
        };
        if observed.source != self.source {
            return Err(TaskFeedError::SourceMismatch);
        }
        let mut receiver = if let Some(group) = self
            .groups
            .iter_mut()
            .find(|g| g.session == session && g.query == query)
        {
            // Same source/revision must identify the same query result. Reject
            // observable untracked writes rather than quietly blessing them.
            if observed.revision.sequence < group.observed.revision.sequence
                || (observed == group.observed
                    && !matches!(&group.sender.borrow().result,
                        TaskResult::Snapshot { tasks } if tasks == &result.tasks))
            {
                publish(group, TaskResult::ResetRequired, None)?;
                return Err(TaskFeedError::SourceMismatch);
            }
            publish(
                group,
                TaskResult::Snapshot {
                    tasks: result.tasks,
                },
                Some(observed),
            )?;
            group.observed = observed;
            group.sender.subscribe()
        } else {
            let message = Arc::new(TaskMessage {
                visibility: visibility(observed),
                session,
                query,
                cursor: DeliveryCursor {
                    subscription: uuid::Uuid::now_v7(),
                    sequence: 1,
                },
                result: TaskResult::Snapshot {
                    tasks: result.tasks,
                },
            });
            let (sender, receiver) = watch::channel(message);
            self.groups.push(Group {
                session,
                query,
                observed,
                sender,
            });
            receiver
        };
        receiver.mark_changed();
        Ok(TaskSubscription {
            session,
            source: self.source,
            receiver: Some(receiver),
        })
    }

    /// Latest full replacements can skip intermediate states. Reconnecting starts
    /// a new lifecycle from an authoritative snapshot; there is no raw-row replay.
    /// Terminal policy/source failures close lifecycles and reject late results.
    pub async fn refresh(&mut self, store: &mut Store) -> Result<(), TaskFeedError> {
        self.groups.retain(|g| {
            g.sender.receiver_count() > 0
                && matches!(g.sender.borrow().result, TaskResult::Snapshot { .. })
        });
        if let Err(error) = self.check_source(store).await {
            for group in &mut self.groups {
                publish(group, TaskResult::ResetRequired, None)?;
            }
            self.groups.clear();
            return Err(error);
        }
        let mut failure = None;
        for group in &mut self.groups {
            let observed = match store.managed_head(group.session).await {
                Ok(head) => head,
                Err(hulladb_postgres::Error::Forbidden) => {
                    publish(group, TaskResult::Revoked, None)?;
                    continue;
                }
                Err(error) => {
                    publish(group, TaskResult::ResetRequired, None)?;
                    failure = Some(TaskFeedError::Authorization(error));
                    continue;
                }
            };
            if observed.source != self.source
                || observed.revision.sequence < group.observed.revision.sequence
            {
                publish(group, TaskResult::ResetRequired, None)?;
                failure = Some(TaskFeedError::SourceMismatch);
            } else if observed != group.observed {
                match store.query_snapshot(group.session, group.query).await {
                    Ok(result) => {
                        // Use the revision paired with these rows, not the earlier
                        // polling observation: a concurrent commit may be included.
                        let head = ManagedHead {
                            source: result.source,
                            revision: result.revision,
                        };
                        if head.source != self.source
                            || head.revision.sequence < observed.revision.sequence
                        {
                            publish(group, TaskResult::ResetRequired, None)?;
                            failure = Some(TaskFeedError::SourceMismatch);
                            continue;
                        }
                        publish(
                            group,
                            TaskResult::Snapshot {
                                tasks: result.tasks,
                            },
                            Some(head),
                        )?;
                        group.observed = head;
                    }
                    Err(hulladb_postgres::Error::Forbidden) => {
                        publish(group, TaskResult::Revoked, None)?;
                    }
                    Err(error) => {
                        publish(group, TaskResult::ResetRequired, None)?;
                        failure = Some(TaskFeedError::Query(error));
                    }
                }
            }
        }
        self.groups
            .retain(|g| matches!(g.sender.borrow().result, TaskResult::Snapshot { .. }));
        failure.map_or(Ok(()), Err)
    }

    async fn check_source(&self, store: &Store) -> Result<(), TaskFeedError> {
        if store.managed_source().await.map_err(TaskFeedError::Query)? != self.source {
            return Err(TaskFeedError::SourceMismatch);
        }
        Ok(())
    }
}

impl TaskSubscription {
    /// Wait without occupying a database connection. Leaves the replacement
    /// pending until next rechecks authorization; cancellation cannot consume it.
    pub async fn ready(&mut self) -> Result<(), TaskFeedError> {
        let receiver = self.receiver.as_mut().ok_or(TaskFeedError::Closed)?;
        receiver
            .changed()
            .await
            .map_err(|_| TaskFeedError::Closed)?;
        receiver.mark_changed();
        Ok(())
    }

    /// Recheck current permission immediately before releasing a queued result.
    /// Cancellation during that check retains the message for retry.
    pub async fn next(&mut self, store: &Store) -> Result<Arc<TaskMessage>, TaskFeedError> {
        self.ready().await?;
        let receiver = self.receiver.as_mut().ok_or(TaskFeedError::Closed)?;
        match store.managed_head(self.session).await {
            Ok(head) if head.source == self.source => {}
            Ok(_) => {
                let queued = receiver.borrow().clone();
                let message = Arc::new(TaskMessage {
                    result: TaskResult::ResetRequired,
                    ..(*queued).clone()
                });
                self.receiver = None;
                return Ok(message);
            }
            Err(hulladb_postgres::Error::Forbidden) => {
                let queued = receiver.borrow().clone();
                let message = Arc::new(TaskMessage {
                    session: queued.session,
                    query: queued.query,
                    cursor: queued.cursor,
                    visibility: queued.visibility,
                    result: TaskResult::Revoked,
                });
                self.receiver = None;
                return Ok(message);
            }
            Err(error) => {
                self.receiver = None;
                return Err(TaskFeedError::Authorization(error));
            }
        }
        let message = receiver.borrow_and_update().clone();
        if !matches!(message.result, TaskResult::Snapshot { .. }) {
            self.receiver = None;
        }
        Ok(message)
    }
}

fn query_error(error: hulladb_postgres::Error) -> TaskFeedError {
    if matches!(error, hulladb_postgres::Error::Forbidden) {
        TaskFeedError::Authorization(error)
    } else {
        TaskFeedError::Query(error)
    }
}

fn visibility(head: ManagedHead) -> VisibilityFence {
    VisibilityFence {
        source: head.source,
        workspace: head.revision.workspace,
        sequence: head.revision.sequence,
    }
}

fn publish(
    group: &mut Group,
    result: TaskResult,
    head: Option<ManagedHead>,
) -> Result<(), TaskFeedError> {
    let prior = group.sender.borrow().clone();
    let visibility = head.map(visibility).unwrap_or(prior.visibility);
    if prior.result == result && prior.visibility == visibility {
        return Ok(());
    }
    let sequence = prior
        .cursor
        .sequence
        .checked_add(1)
        .ok_or(TaskFeedError::Limit)?;
    group.sender.send_replace(Arc::new(TaskMessage {
        session: prior.session,
        query: prior.query,
        visibility,
        result,
        cursor: DeliveryCursor {
            sequence,
            ..prior.cursor
        },
    }));
    Ok(())
}
