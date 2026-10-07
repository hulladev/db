//! Native SQLite persistence for authorized task-query deliveries. This crate has
//! no PostgreSQL/CDC dependencies and never connects to the authoritative database.
//! Bootstrap chunks stay unpublished until rows, delivery history, and cursor commit
//! together. Transport authentication and browser persistence are separate adapters.
use std::{fmt, path::Path, time::Duration};

pub use hulladb_protocol::{
    DeliveryCursor, Session, TaskMessage, TaskQuery, TaskResult, TaskSummary, VisibilityFence,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use uuid::Uuid;

/// Caller-selected working and history budgets, independent of total stored data.
/// Byte limits measure encoded chunk payloads, not SQLite files or process RSS.
#[derive(Debug, Clone, Copy)]
pub struct ReplicaOptions {
    pub max_chunk_rows: usize,
    pub max_chunk_bytes: usize,
    pub retained_deliveries_per_query: usize,
    pub busy_timeout: Duration,
}

#[derive(Debug)]
pub enum Error {
    InvalidOptions(&'static str),
    Contract(hulladb_protocol::ContractError),
    ScopeMismatch,
    InvalidCursor,
    InvalidVisibility,
    Closed,
    NotCovered,
    Corrupt,
    Budget {
        resource: &'static str,
        used: usize,
        limit: usize,
    },
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOptions(reason) => f.write_str(reason),
            Self::Contract(error) => fmt::Display::fmt(error, f),
            Self::ScopeMismatch => {
                f.write_str("client database belongs to a different authenticated scope")
            }
            Self::InvalidCursor => f.write_str("delivery lifecycle or sequence differs"),
            Self::InvalidVisibility => {
                f.write_str("visibility source differs or revision regressed")
            }
            Self::Closed => f.write_str("client query lifecycle is closed"),
            Self::NotCovered => f.write_str("query has no complete local snapshot"),
            Self::Corrupt => f.write_str("client delivery violates its query contract"),
            Self::Budget {
                resource,
                used,
                limit,
            } => write!(f, "client {resource} budget exceeded: {used} > {limit}"),
            Self::Sqlite(error) => fmt::Display::fmt(error, f),
            Self::Json(error) => fmt::Display::fmt(error, f),
        }
    }
}
impl std::error::Error for Error {}
impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
impl From<hulladb_protocol::ContractError> for Error {
    fn from(e: hulladb_protocol::ContractError) -> Self {
        Self::Contract(e)
    }
}

pub struct TaskReplica {
    db: Connection,
    session: Session,
    options: ReplicaOptions,
}

#[derive(Debug, PartialEq, Eq)]
pub struct QueryView {
    pub cursor: DeliveryCursor,
    pub visibility: VisibilityFence,
    pub tasks: Vec<TaskSummary>,
}

/// A failed/cancelled receiver drops this transaction, retaining the previous page.
/// Call append with bounded chunks, then finish only after the end-of-snapshot frame.
pub struct SnapshotWriter<'a> {
    tx: Transaction<'a>,
    key: String,
    query: TaskQuery,
    cursor: DeliveryCursor,
    visibility: VisibilityFence,
    options: ReplicaOptions,
    previous: Option<Uuid>,
    count: usize,
    chunk: i64,
    failed: bool,
}

impl TaskReplica {
    pub fn open(
        path: impl AsRef<Path>,
        session: Session,
        options: ReplicaOptions,
    ) -> Result<Self, Error> {
        if options.max_chunk_rows == 0
            || options.max_chunk_bytes == 0
            || options.retained_deliveries_per_query > i64::MAX as usize
        {
            return Err(Error::InvalidOptions(
                "chunk budgets must be positive and retained deliveries must fit SQLite integers",
            ));
        }
        let mut db = Connection::open(path)?;
        db.busy_timeout(options.busy_timeout)?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        let mode: String = db.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
        let synchronous: i64 = db.query_row("PRAGMA synchronous", [], |r| r.get(0))?;
        if mode != "wal" || synchronous != 2 {
            return Err(Error::Corrupt);
        }
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS client_metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1),session TEXT NOT NULL,revoked INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS client_queries(query_key TEXT PRIMARY KEY,subscription TEXT NOT NULL,sequence INTEGER NOT NULL,ready INTEGER NOT NULL,active INTEGER NOT NULL,visibility TEXT);
            CREATE TABLE IF NOT EXISTS client_tasks(query_key TEXT NOT NULL REFERENCES client_queries(query_key) ON DELETE CASCADE,id TEXT NOT NULL,title TEXT NOT NULL,completed INTEGER NOT NULL,PRIMARY KEY(query_key,id));
            CREATE TABLE IF NOT EXISTS client_events(query_key TEXT NOT NULL REFERENCES client_queries(query_key) ON DELETE CASCADE,sequence INTEGER NOT NULL,chunk INTEGER NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(query_key,sequence,chunk));")?;
        tx.execute(
            "INSERT OR IGNORE INTO client_metadata VALUES(1,?1,0)",
            [serde_json::to_string(&session)?],
        )?;
        let (stored, revoked): (String, bool) = tx.query_row(
            "SELECT session,revoked FROM client_metadata WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if serde_json::from_str::<Session>(&stored)? != session {
            return Err(Error::ScopeMismatch);
        }
        if revoked {
            return Err(Error::Closed);
        }
        tx.commit()?;
        Ok(Self {
            db,
            session,
            options,
        })
    }

    /// Register the lifecycle returned by an authenticated subscription handshake.
    /// A new subscription ID explicitly replaces this query's old coverage/history.
    pub fn register(&mut self, query: TaskQuery, subscription: Uuid) -> Result<(), Error> {
        query.validate()?;
        self.ensure_open()?;
        let key = serde_json::to_string(&query)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<(String, bool)> = tx
            .query_row(
                "SELECT subscription,active FROM client_queries WHERE query_key=?1",
                [&key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((stored, active)) = existing {
            if stored == subscription.to_string() {
                return if active { Ok(()) } else { Err(Error::Closed) };
            }
            tx.execute("DELETE FROM client_queries WHERE query_key=?1", [&key])?;
        }
        tx.execute(
            "INSERT INTO client_queries VALUES(?1,?2,0,0,1,NULL)",
            params![key, subscription.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn begin_snapshot(
        &mut self,
        query: TaskQuery,
        cursor: DeliveryCursor,
        visibility: VisibilityFence,
    ) -> Result<SnapshotWriter<'_>, Error> {
        query.validate()?;
        visibility.validate(self.session.workspace)?;
        self.ensure_open()?;
        let key = serde_json::to_string(&query)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous = check_cursor(&tx, &key, cursor)?;
        if cursor.sequence <= previous {
            return Err(Error::InvalidCursor);
        }
        let stored: Option<String> = tx.query_row(
            "SELECT visibility FROM client_queries WHERE query_key=?1",
            [&key],
            |r| r.get(0),
        )?;
        if let Some(stored) = stored {
            let prior: VisibilityFence = serde_json::from_str(&stored)?;
            if prior.source != visibility.source || prior.sequence > visibility.sequence {
                return Err(Error::InvalidVisibility);
            }
        }
        // WAL readers continue seeing the prior committed page during streamed writes.
        tx.execute("DELETE FROM client_tasks WHERE query_key=?1", [&key])?;
        Ok(SnapshotWriter {
            tx,
            key,
            query,
            cursor,
            visibility,
            options: self.options,
            previous: query.after,
            count: 0,
            chunk: 0,
            failed: false,
        })
    }

    /// Apply a complete coalesced page using the same writer as streamed bootstrap.
    /// No requirement to replay skipped delivery sequences for full replacements.
    pub fn apply(&mut self, message: &TaskMessage) -> Result<(), Error> {
        if message.session != self.session {
            return Err(Error::ScopeMismatch);
        }
        message.query.validate()?;
        self.ensure_open()?;
        match &message.result {
            TaskResult::Snapshot { tasks } => {
                if let Ok(existing) = self.read(self.session, message.query)
                    && existing.cursor == message.cursor
                {
                    return if existing.tasks == *tasks && existing.visibility == message.visibility
                    {
                        Ok(())
                    } else {
                        Err(Error::Corrupt)
                    };
                }
                let mut writer =
                    self.begin_snapshot(message.query, message.cursor, message.visibility)?;
                // Individual rows are already domain bounded. Network adapters can
                // append larger chunks up to the caller-selected byte/row budgets.
                for task in tasks {
                    writer.append(std::slice::from_ref(task))?;
                }
                writer.finish()
            }
            TaskResult::Revoked | TaskResult::ResetRequired => {
                let key = serde_json::to_string(&message.query)?;
                let tx = self
                    .db
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                check_cursor(&tx, &key, message.cursor)?;
                if matches!(message.result, TaskResult::Revoked) {
                    // Membership is workspace scoped: erase all cached query data,
                    // including recent event payloads, and reject late deliveries.
                    tx.execute("DELETE FROM client_queries", [])?;
                    tx.execute("UPDATE client_metadata SET revoked=1 WHERE singleton=1", [])?;
                } else {
                    tx.execute("DELETE FROM client_tasks WHERE query_key=?1", [&key])?;
                    tx.execute("DELETE FROM client_events WHERE query_key=?1", [&key])?;
                    tx.execute(
                        "UPDATE client_queries SET ready=0,active=0,sequence=?2 WHERE query_key=?1",
                        params![key, message.cursor.sequence],
                    )?;
                }
                tx.commit()?;
                Ok(())
            }
        }
    }

    /// Read only an exactly registered query, including an authoritative empty page.
    /// Historical coverage survives reopening; connectivity/freshness is transport state.
    pub fn read(&self, session: Session, query: TaskQuery) -> Result<QueryView, Error> {
        if session != self.session {
            return Err(Error::ScopeMismatch);
        }
        query.validate()?;
        self.ensure_open()?;
        let key = serde_json::to_string(&query)?;
        let tx = self.db.unchecked_transaction()?;
        let (cursor, visibility) = progress(&tx, &key, self.session.workspace)?;
        let mut statement = tx.prepare(
            "SELECT id,title,completed FROM client_tasks WHERE query_key=?1 ORDER BY id",
        )?;
        let rows = statement.query_map([key], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?,
            ))
        })?;
        let mut tasks = Vec::new();
        let mut previous = query.after;
        for row in rows {
            let (id, title, completed) = row?;
            let task = TaskSummary {
                id: id.parse().map_err(|_| Error::Corrupt)?,
                title,
                completed,
            };
            task.validate()?;
            if previous.is_some_and(|id| id >= task.id)
                || query.completed.is_some_and(|done| done != task.completed)
                || tasks.len() >= usize::from(query.limit)
            {
                return Err(Error::Corrupt);
            }
            previous = Some(task.id);
            tasks.push(task);
        }
        Ok(QueryView {
            visibility,
            cursor,
            tasks,
        })
    }

    /// True only when this exact query's committed SQLite snapshot was evaluated
    /// at or beyond the receipt revision. This does not promise present freshness
    /// or that the changed row belongs to the query. Missing/closed coverage errors.
    pub fn is_visible(
        &self,
        session: Session,
        query: TaskQuery,
        fence: VisibilityFence,
    ) -> Result<bool, Error> {
        fence.validate(self.session.workspace)?;
        if session != self.session {
            return Err(Error::ScopeMismatch);
        }
        query.validate()?;
        self.ensure_open()?;
        let tx = self.db.unchecked_transaction()?;
        let (_, visibility) =
            progress(&tx, &serde_json::to_string(&query)?, self.session.workspace)?;
        if visibility.source != fence.source {
            return Err(Error::InvalidVisibility);
        }
        Ok(visibility.sequence >= fence.sequence)
    }

    fn ensure_open(&self) -> Result<(), Error> {
        let revoked: bool = self.db.query_row(
            "SELECT revoked FROM client_metadata WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if revoked { Err(Error::Closed) } else { Ok(()) }
    }
}

fn progress(
    tx: &Transaction<'_>,
    key: &str,
    workspace: Uuid,
) -> Result<(DeliveryCursor, VisibilityFence), Error> {
    type Metadata = (String, i64, bool, bool, Option<String>);
    let metadata: Option<Metadata> = tx
            .query_row(
                "SELECT subscription,sequence,ready,active,visibility FROM client_queries WHERE query_key=?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
    let (subscription, sequence, ready, active, visibility) = metadata.ok_or(Error::NotCovered)?;
    if !active {
        return Err(Error::Closed);
    }
    if !ready {
        return Err(Error::NotCovered);
    }
    let visibility: VisibilityFence = serde_json::from_str(&visibility.ok_or(Error::Corrupt)?)?;
    visibility.validate(workspace)?;
    Ok((
        DeliveryCursor {
            subscription: subscription.parse().map_err(|_| Error::Corrupt)?,
            sequence,
        },
        visibility,
    ))
}

fn check_cursor(tx: &Transaction<'_>, key: &str, cursor: DeliveryCursor) -> Result<i64, Error> {
    let stored: Option<(String, i64, bool)> = tx
        .query_row(
            "SELECT subscription,sequence,active FROM client_queries WHERE query_key=?1",
            [key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (subscription, sequence, active) = stored.ok_or(Error::NotCovered)?;
    if !active {
        return Err(Error::Closed);
    }
    if subscription != cursor.subscription.to_string()
        || cursor.sequence <= 0
        || cursor.sequence < sequence
    {
        return Err(Error::InvalidCursor);
    }
    Ok(sequence)
}

impl SnapshotWriter<'_> {
    pub fn append(&mut self, rows: &[TaskSummary]) -> Result<(), Error> {
        if self.failed {
            return Err(Error::Corrupt);
        }
        let result = self.append_checked(rows);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn append_checked(&mut self, rows: &[TaskSummary]) -> Result<(), Error> {
        if rows.len() > self.options.max_chunk_rows {
            return Err(Error::Budget {
                resource: "chunk rows",
                used: rows.len(),
                limit: self.options.max_chunk_rows,
            });
        }
        if self.count + rows.len() > usize::from(self.query.limit) {
            return Err(Error::Corrupt);
        }
        let mut previous = self.previous;
        let mut bytes = 2 + rows.len().saturating_sub(1);
        if bytes > self.options.max_chunk_bytes {
            return Err(Error::Budget {
                resource: "chunk bytes",
                used: bytes,
                limit: self.options.max_chunk_bytes,
            });
        }
        for task in rows {
            task.validate()?;
            if previous.is_some_and(|id| id >= task.id)
                || self
                    .query
                    .completed
                    .is_some_and(|done| done != task.completed)
            {
                return Err(Error::Corrupt);
            }
            previous = Some(task.id);
            bytes = bytes
                .checked_add(serde_json::to_vec(task)?.len())
                .ok_or(Error::Corrupt)?;
            if bytes > self.options.max_chunk_bytes {
                return Err(Error::Budget {
                    resource: "chunk bytes",
                    used: bytes,
                    limit: self.options.max_chunk_bytes,
                });
            }
        }
        let mut statement = self
            .tx
            .prepare("INSERT INTO client_tasks VALUES(?1,?2,?3,?4)")?;
        for task in rows {
            statement.execute(params![
                self.key,
                task.id.to_string(),
                task.title,
                task.completed
            ])?;
        }
        if self.options.retained_deliveries_per_query > 0 {
            self.tx.execute(
                "INSERT INTO client_events VALUES(?1,?2,?3,?4)",
                params![
                    self.key,
                    self.cursor.sequence,
                    self.chunk,
                    serde_json::to_string(rows)?
                ],
            )?;
        }
        self.previous = previous;
        self.count += rows.len();
        self.chunk += 1;
        Ok(())
    }

    /// The end frame is the only publication point, including for an empty page.
    pub fn finish(self) -> Result<(), Error> {
        if self.failed {
            return Err(Error::Corrupt);
        }
        if self.chunk == 0 && self.options.retained_deliveries_per_query > 0 {
            if self.options.max_chunk_bytes < 2 {
                return Err(Error::Budget {
                    resource: "chunk bytes",
                    used: 2,
                    limit: self.options.max_chunk_bytes,
                });
            }
            self.tx.execute(
                "INSERT INTO client_events VALUES(?1,?2,0,'[]')",
                params![self.key, self.cursor.sequence],
            )?;
        }
        self.tx.execute(
            "UPDATE client_queries SET sequence=?2,ready=1,visibility=?3 WHERE query_key=?1",
            params![
                self.key,
                self.cursor.sequence,
                serde_json::to_string(&self.visibility)?
            ],
        )?;
        self.tx.execute("DELETE FROM client_events WHERE query_key=?1 AND sequence NOT IN (SELECT DISTINCT sequence FROM client_events WHERE query_key=?1 ORDER BY sequence DESC LIMIT ?2)", params![self.key,self.options.retained_deliveries_per_query as i64])?;
        self.tx.commit()?;
        Ok(())
    }
}
