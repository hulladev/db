use std::sync::OnceLock;

use hulladb_schema::{ProjectionContract, RecordValues, Value};
use serde::{Deserialize, Serialize};

use crate::{
    Error, MAX_PAGE, QueryRevision, Session, Uuid,
    model::{task_contract, validate_title},
};

/// The fixed task-summary query: ID order, optional status/keyset bound, explicit limit.
/// Its entity projection is always id/title/completed; version is not delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskQuery {
    pub completed: Option<bool>,
    pub after: Option<Uuid>,
    pub limit: u16,
}

impl TaskQuery {
    pub(crate) fn validate(self) -> Result<(), Error> {
        if self.limit == 0 || self.limit > MAX_PAGE {
            return Err(Error::InvalidInput("query limit must be 1..100"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSummary {
    pub id: Uuid,
    pub title: String,
    pub completed: bool,
}

impl TaskSummary {
    pub fn values(&self) -> RecordValues {
        RecordValues::from([
            ("id".into(), Value::Uuid(*self.id.as_bytes())),
            ("title".into(), Value::String(self.title.clone())),
            ("completed".into(), Value::Boolean(self.completed)),
        ])
    }

    pub(crate) fn validate(&self) -> Result<(), Error> {
        static PROJECTION: OnceLock<ProjectionContract> = OnceLock::new();
        validate_title(&self.title)?;
        PROJECTION
            .get_or_init(|| {
                task_contract()
                    .projection("summary", &["id", "title", "completed"])
                    .expect("fixed projection is valid")
            })
            .validate_record(&self.values())
            .map_err(|_| Error::CorruptData)
    }
}

/// An authoritative result complete for THIS bounded query at its managed-query revision.
/// It makes no completeness claim about the underlying workspace or other queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuerySnapshot {
    pub session: Session,
    pub query: TaskQuery,
    pub revision: QueryRevision,
    pub tasks: Vec<TaskSummary>,
}

impl QuerySnapshot {
    fn validate(&self) -> Result<(), Error> {
        self.query.validate()?;
        self.revision.validate(self.session.workspace)?;
        if self.tasks.len() > usize::from(self.query.limit) {
            return Err(Error::CorruptData);
        }
        let mut previous = self.query.after;
        for task in &self.tasks {
            task.validate()?;
            if previous.is_some_and(|id| id >= task.id)
                || self
                    .query
                    .completed
                    .is_some_and(|done| done != task.completed)
            {
                return Err(Error::CorruptData);
            }
            previous = Some(task.id);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryStatus {
    Loading,
    Ready,
    Stale,
    Disconnected,
    Failed,
}

pub struct QueryView<'a> {
    pub tasks: &'a [TaskSummary],
    /// Historical coverage proof, independent of current connectivity/loading/errors.
    pub complete_at: Option<QueryRevision>,
    pub observed_at: Option<QueryRevision>,
    pub status: QueryStatus,
}

/// One exact query, projection, principal, workspace, and schema generation.
/// No inferred coverage across filters, limits, pages, or account identities.
pub struct QueryCache {
    session: Session,
    query: TaskQuery,
    snapshot: Option<QuerySnapshot>,
    observed: Option<QueryRevision>,
    status: QueryStatus,
    revoked: bool,
}

impl QueryCache {
    pub fn new(session: Session, query: TaskQuery) -> Result<Self, Error> {
        query.validate()?;
        Ok(Self {
            session,
            query,
            snapshot: None,
            observed: None,
            status: QueryStatus::Loading,
            revoked: false,
        })
    }

    pub fn read(&self, session: Session, query: TaskQuery) -> Result<QueryView<'_>, Error> {
        if session != self.session {
            return Err(Error::Forbidden);
        }
        if query != self.query {
            return Err(Error::QueryNotCovered);
        }
        Ok(QueryView {
            tasks: self
                .snapshot
                .as_ref()
                .map_or(&[], |snapshot| snapshot.tasks.as_slice()),
            complete_at: self.snapshot.as_ref().map(|snapshot| snapshot.revision),
            observed_at: self.observed,
            status: self.status,
        })
    }

    /// Publish a validated bounded result atomically; a delayed response cannot regress it.
    pub fn install(&mut self, snapshot: QuerySnapshot) -> Result<(), Error> {
        if self.revoked {
            return Err(Error::Forbidden);
        }
        if snapshot.session != self.session {
            return Err(Error::Forbidden);
        }
        if snapshot.query != self.query {
            return Err(Error::QueryNotCovered);
        }
        snapshot.validate()?;
        if let Some(previous) = &self.snapshot {
            if snapshot.revision.sequence < previous.revision.sequence {
                return Err(Error::InvalidQueryRevision);
            }
            if snapshot.revision == previous.revision && snapshot.tasks != previous.tasks {
                return Err(Error::CorruptData);
            }
        }
        let head = self
            .observed
            .map_or(snapshot.revision.sequence, |revision| revision.sequence)
            .max(snapshot.revision.sequence);
        self.observed = Some(QueryRevision {
            sequence: head,
            ..snapshot.revision
        });
        if self.status != QueryStatus::Disconnected {
            self.status = if snapshot.revision.sequence < head {
                QueryStatus::Stale
            } else {
                QueryStatus::Ready
            };
        }
        self.snapshot = Some(snapshot);
        Ok(())
    }

    /// Conservatively invalidate on every observed workspace commit. A result-only
    /// delta cannot refill top-N membership; fetch another bounded snapshot.
    pub fn observe(&mut self, revision: QueryRevision) -> Result<(), Error> {
        if self.revoked {
            return Err(Error::Forbidden);
        }
        revision.validate(self.session.workspace)?;
        if self
            .observed
            .is_none_or(|previous| revision.sequence > previous.sequence)
        {
            self.observed = Some(revision);
            if self.status == QueryStatus::Ready {
                self.status = QueryStatus::Stale;
            }
        }
        Ok(())
    }

    pub fn begin_refresh(&mut self) -> Result<(), Error> {
        if self.revoked {
            return Err(Error::Forbidden);
        }
        self.status = QueryStatus::Loading;
        Ok(())
    }

    pub fn disconnect(&mut self) {
        if !self.revoked {
            self.status = QueryStatus::Disconnected;
        }
    }

    /// Permission failure clears retained data and closes this cache. A new
    /// authenticated lifecycle must create a new cache, rejecting late responses.
    pub fn fail(&mut self, error: &Error) {
        self.status = QueryStatus::Failed;
        if matches!(error, Error::Forbidden) {
            self.snapshot = None;
            self.observed = None;
            self.revoked = true;
        }
    }
}
