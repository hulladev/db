use std::{path::Path, time::Duration};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{Batch, Change, Cursor, Error, Event, Lsn, Source};

mod reader;
mod staging;
use reader::same_chunks;
pub use reader::{StoredTransaction, TransactionHeader};
use staging::{Staging, flush_chunk, new_stage, write_change};

#[derive(Debug, Clone, Copy)]
pub struct Retention {
    pub max_batches: u32,
    pub max_bytes: u64,
}
impl Default for Retention {
    fn default() -> Self {
        Self {
            max_batches: 128,
            max_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Ingestion working budget, independent of the size of a committed transaction.
/// The optional staging quota measures encoded changes, not SQLite/WAL disk usage.
#[derive(Debug, Clone, Copy)]
pub struct StorageOptions {
    pub chunk_bytes: usize,
    pub max_staged_payload_bytes: Option<u64>,
    pub busy_timeout: Duration,
}
impl Default for StorageOptions {
    fn default() -> Self {
        Self {
            chunk_bytes: 256 * 1024,
            max_staged_payload_bytes: None,
            busy_timeout: Duration::from_secs(5),
        }
    }
}

/// Only the convenience API that collects complete transactions into a Vec uses
/// this budget. Streaming ingestion/readers have no total transaction-size ceiling.
#[derive(Debug, Clone, Copy)]
pub struct ReadBudget {
    pub max_bytes: u64,
    pub max_changes: u64,
}
impl Default for ReadBudget {
    fn default() -> Self {
        Self {
            max_bytes: 4 * 1024 * 1024,
            max_changes: 10_000,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StorageStats {
    pub batches: u64,
    pub payload_bytes: u64,
    pub staged_payload_bytes: u64,
    pub database_bytes: u64,
    pub reusable_bytes: u64,
}

/// Single ingestion owner, with independent SQLite readers. Staging rows/chunks
/// are durable but invisible to delivery readers until their sequence is published.
/// A new owner discards unfinished staging and replays from the durable checkpoint.
pub struct EventLog {
    db: Connection,
    source: Source,
    retention: Retention,
    options: StorageOptions,
    staging: Option<Staging>,
}

const CHUNK_SCHEMA: &str = "CREATE TABLE batches(id TEXT PRIMARY KEY,sequence INTEGER UNIQUE,xid INTEGER NOT NULL,commit_lsn TEXT NOT NULL,end_lsn TEXT UNIQUE,change_count INTEGER NOT NULL,payload_bytes INTEGER NOT NULL,chunk_count INTEGER NOT NULL);
    CREATE TABLE batch_chunks(batch_id TEXT NOT NULL REFERENCES batches(id) ON DELETE CASCADE,ordinal INTEGER NOT NULL,payload BLOB NOT NULL,PRIMARY KEY(batch_id,ordinal));";

impl EventLog {
    pub fn open(
        path: impl AsRef<Path>,
        source: Source,
        retention: Retention,
    ) -> Result<Self, Error> {
        Self::open_with_options(path, source, retention, StorageOptions::default())
    }

    pub fn open_with_options(
        path: impl AsRef<Path>,
        source: Source,
        retention: Retention,
        options: StorageOptions,
    ) -> Result<Self, Error> {
        if retention.max_batches == 0
            || retention.max_batches > 100_000
            || retention.max_bytes == 0
            || retention.max_bytes > i64::MAX as u64
        {
            return Err(Error::Invalid("invalid retention bounds"));
        }
        if options.chunk_bytes == 0
            || options.chunk_bytes > i32::MAX as usize
            || options
                .max_staged_payload_bytes
                .is_some_and(|n| n == 0 || n > i64::MAX as u64)
        {
            return Err(Error::Invalid("invalid staging budget"));
        }
        let mut db = Connection::open(path)?;
        db.busy_timeout(options.busy_timeout)?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        let mode: String = db.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
        let synchronous: i64 = db.query_row("PRAGMA synchronous", [], |r| r.get(0))?;
        if mode != "wal" || synchronous != 2 {
            return Err(Error::Invalid(
                "event storage requires durable WAL/FULL mode",
            ));
        }
        db.execute_batch("CREATE TABLE IF NOT EXISTS metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1),source TEXT NOT NULL,checkpoint TEXT NOT NULL,head INTEGER NOT NULL,floor INTEGER NOT NULL);")?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool =
            tx.query_row("SELECT EXISTS(SELECT 1 FROM metadata)", [], |r| r.get(0))?;
        if !exists {
            tx.execute_batch(CHUNK_SCHEMA)?;
            tx.execute_batch("CREATE TABLE event_usage(singleton INTEGER PRIMARY KEY CHECK(singleton=1),batches INTEGER NOT NULL CHECK(batches>=0),bytes INTEGER NOT NULL CHECK(bytes>=0)); INSERT INTO event_usage VALUES(1,0,0);")?;
            tx.execute(
                "INSERT INTO metadata VALUES(1,?1,?2,0,0)",
                params![serde_json::to_string(&source)?, source.start.to_string()],
            )?;
        }
        let stored: String =
            tx.query_row("SELECT source FROM metadata WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
        if serde_json::from_str::<Source>(&stored)? != source {
            return Err(Error::SourceMismatch);
        }
        tx.query_row(
            "SELECT batches,bytes FROM event_usage WHERE singleton=1",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )?;
        tx.commit()?;
        Ok(Self {
            db,
            source,
            retention,
            options,
            staging: None,
        })
    }

    pub fn source(&self) -> &Source {
        &self.source
    }
    pub fn checkpoint(&self) -> Result<Lsn, Error> {
        self.db
            .query_row(
                "SELECT checkpoint FROM metadata WHERE singleton=1",
                [],
                |r| r.get::<_, String>(0),
            )?
            .parse()
    }
    pub fn head(&self) -> Result<Cursor, Error> {
        Ok(Cursor {
            source_id: self.source.source_id,
            sequence: self.db.query_row(
                "SELECT head FROM metadata WHERE singleton=1",
                [],
                |r| r.get(0),
            )?,
        })
    }

    pub(crate) fn is_staging(&self) -> bool {
        self.staging.is_some()
    }

    /// Called only by the sole ingestion owner, never by read-only open/inspection.
    pub(crate) fn abort_staging(&mut self) -> Result<(), Error> {
        self.staging = None;
        self.db
            .execute("DELETE FROM batches WHERE sequence IS NULL", [])?;
        Ok(())
    }

    pub(crate) fn begin_transaction(&mut self, xid: u32, commit: Lsn) -> Result<(), Error> {
        if self.is_staging() {
            return Err(Error::Protocol("transaction already staged"));
        }
        if commit < self.source.start {
            return Err(Error::Invalid("transaction predates source"));
        }
        // Crash leftovers have no published sequence and no acknowledged boundary.
        self.abort_staging()?;
        self.staging = Some(new_stage(xid, commit));
        Ok(())
    }

    pub(crate) fn push_change(&mut self, change: &Change) -> Result<(), Error> {
        let stage = self
            .staging
            .as_mut()
            .ok_or(Error::Protocol("change without staged BEGIN"))?;
        if stage.failed {
            return Err(Error::Corrupt);
        }
        let result = write_change(&self.db, stage, self.options, change);
        if result.is_err() {
            stage.failed = true;
        }
        result
    }

    pub(crate) fn finish_transaction(
        &mut self,
        commit: Lsn,
        end: Lsn,
    ) -> Result<(Option<Cursor>, bool), Error> {
        let mut stage = self
            .staging
            .take()
            .ok_or(Error::Protocol("COMMIT without staging"))?;
        let result = (|| {
            if stage.failed || stage.commit != commit || end <= commit {
                return Err(Error::Corrupt);
            }
            let tx = self
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Small transactions write their only chunk in this publication
            // transaction: one FULL commit, without extra staging fsyncs.
            flush_chunk(&tx, &mut stage)?;
            let (checkpoint, head, mut count, mut bytes): (String, i64, i64, i64) = tx.query_row(
                "SELECT checkpoint,head,event_usage.batches,event_usage.bytes FROM metadata JOIN event_usage USING(singleton) WHERE singleton=1",
                [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            let checkpoint: Lsn = checkpoint.parse()?;
            if end <= checkpoint {
                let previous: Option<(String,u32,String,i64,i64)> = tx.query_row(
                    "SELECT id,xid,commit_lsn,change_count,chunk_count FROM batches WHERE end_lsn=?1 AND sequence IS NOT NULL", [end.to_string()],
                    |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
                if let Some((id, xid, old_commit, changes, chunks)) = previous
                    && (xid != stage.xid
                        || old_commit.parse::<Lsn>()? != commit
                        || changes != stage.changes as i64
                        || !same_chunks(&tx, &id, chunks, &stage.id, stage.chunks)?)
                {
                    return Err(Error::Corrupt);
                }
                tx.execute("DELETE FROM batches WHERE id=?1", [&stage.id])?;
                tx.commit()?;
                return Ok((None, stage.changes > 0));
            }
            if commit < checkpoint {
                return Err(Error::Corrupt);
            }
            if stage.changes == 0 {
                tx.execute("DELETE FROM batches WHERE id=?1", [&stage.id])?;
                tx.execute(
                    "UPDATE metadata SET checkpoint=?1 WHERE singleton=1",
                    [end.to_string()],
                )?;
                tx.commit()?;
                return Ok((None, false));
            }
            let sequence = head.checked_add(1).ok_or(Error::Corrupt)?;
            tx.execute(
                "UPDATE batches SET sequence=?2,end_lsn=?3,change_count=?4 WHERE id=?1",
                params![stage.id, sequence, end.to_string(), stage.changes as i64],
            )?;
            tx.execute(
                "UPDATE metadata SET checkpoint=?1,head=?2 WHERE singleton=1",
                params![end.to_string(), sequence],
            )?;
            count = count.checked_add(1).ok_or(Error::Corrupt)?;
            bytes = bytes
                .checked_add(stage.bytes as i64)
                .ok_or(Error::Corrupt)?;
            let mut floor = None;
            while count > i64::from(self.retention.max_batches)
                || bytes > self.retention.max_bytes as i64
            {
                let (oldest,removed): (i64,i64) = tx.query_row(
                    "DELETE FROM batches WHERE sequence=(SELECT min(sequence) FROM batches WHERE sequence IS NOT NULL) RETURNING sequence,payload_bytes",
                    [], |r| Ok((r.get(0)?,r.get(1)?)))?;
                count = count
                    .checked_sub(1)
                    .filter(|n| *n >= 0)
                    .ok_or(Error::Corrupt)?;
                bytes = bytes
                    .checked_sub(removed)
                    .filter(|n| *n >= 0)
                    .ok_or(Error::Corrupt)?;
                floor = Some(oldest);
            }
            if let Some(floor) = floor {
                tx.execute("UPDATE metadata SET floor=?1 WHERE singleton=1", [floor])?;
            }
            tx.execute(
                "UPDATE event_usage SET batches=?1,bytes=?2 WHERE singleton=1",
                params![count, bytes],
            )?;
            tx.commit()?;
            Ok((
                Some(Cursor {
                    source_id: self.source.source_id,
                    sequence,
                }),
                false,
            ))
        })();
        if result.is_err() {
            // Failure to reclaim disk never changes the durable checkpoint. A
            // later owner retries cleanup before replaying the source transaction.
            let _ = self.abort_staging();
        }
        result
    }

    /// Complete-batch convenience writer; capture uses the incremental methods.
    pub fn append(&mut self, batch: &Batch) -> Result<Option<Cursor>, Error> {
        if batch.commit >= batch.end {
            return Err(Error::Invalid("invalid transaction boundary"));
        }
        self.begin_transaction(batch.xid, batch.commit)?;
        let result = (|| {
            for change in &batch.changes {
                self.push_change(change)?;
            }
            self.finish_transaction(batch.commit, batch.end)
                .map(|(cursor, _)| cursor)
        })();
        if result.is_err() {
            let _ = self.abort_staging();
        }
        result
    }

    pub(crate) fn scanned_through(&mut self, position: Lsn) -> Result<(), Error> {
        if self.is_staging() {
            return Err(Error::Protocol("scan boundary inside staged transaction"));
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: String = tx.query_row(
            "SELECT checkpoint FROM metadata WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if position > current.parse::<Lsn>()? {
            tx.execute(
                "UPDATE metadata SET checkpoint=?1 WHERE singleton=1",
                [position.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Read complete transactions inside one snapshot, without collecting their
    /// changes. Callback failure stops at that transaction; no log state changes.
    pub fn visit_transactions(
        &self,
        cursor: Cursor,
        limit: u32,
        mut visit: impl FnMut(&StoredTransaction<'_>) -> Result<(), Error>,
    ) -> Result<Cursor, Error> {
        if limit == 0 || limit > 100 {
            return Err(Error::Invalid("invalid page limit"));
        }
        let tx = self.db.unchecked_transaction()?;
        let (floor, head): (i64, i64) = tx.query_row(
            "SELECT floor,head FROM metadata WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if cursor.source_id != self.source.source_id
            || cursor.sequence < 0
            || cursor.sequence > head
        {
            return Err(Error::InvalidCursor);
        }
        if cursor.sequence < floor {
            return Err(Error::CursorExpired);
        }
        let mut q = tx.prepare("SELECT id,sequence,xid,commit_lsn,end_lsn,change_count,payload_bytes,chunk_count FROM batches WHERE sequence>?1 ORDER BY sequence LIMIT ?2")?;
        let mut rows = q.query(params![cursor.sequence, limit])?;
        let mut previous = cursor;
        while let Some(row) = rows.next()? {
            let sequence: i64 = row.get(1)?;
            if previous.sequence.checked_add(1) != Some(sequence) {
                return Err(Error::Corrupt);
            }
            let changes: i64 = row.get(5)?;
            let bytes: i64 = row.get(6)?;
            let chunks: i64 = row.get(7)?;
            if changes < 0 || bytes < 0 || chunks < 0 {
                return Err(Error::Corrupt);
            }
            let stored = StoredTransaction {
                header: TransactionHeader {
                    cursor: Cursor {
                        source_id: self.source.source_id,
                        sequence,
                    },
                    xid: row.get(2)?,
                    commit: row.get::<_, String>(3)?.parse()?,
                    end: row.get::<_, String>(4)?.parse()?,
                    changes: changes as u64,
                    payload_bytes: bytes as u64,
                },
                db: &tx,
                id: row.get(0)?,
                chunks,
            };
            visit(&stored)?;
            previous = stored.header.cursor;
        }
        Ok(previous)
    }

    pub fn since(&self, cursor: Cursor, limit: u32) -> Result<Vec<Event>, Error> {
        self.since_with_budget(cursor, limit, ReadBudget::default())
    }
    /// The budget applies to the whole returned page, not separately to each batch.
    pub fn since_with_budget(
        &self,
        cursor: Cursor,
        limit: u32,
        budget: ReadBudget,
    ) -> Result<Vec<Event>, Error> {
        let mut events = Vec::new();
        let mut bytes = 0_u64;
        let mut changes = 0_u64;
        self.visit_transactions(cursor, limit, |stored| {
            bytes = bytes
                .checked_add(stored.header.payload_bytes)
                .ok_or(Error::Corrupt)?;
            changes = changes
                .checked_add(stored.header.changes)
                .ok_or(Error::Corrupt)?;
            if bytes > budget.max_bytes || changes > budget.max_changes {
                return Err(Error::ReadBudget);
            }
            let mut batch = Batch {
                xid: stored.header.xid,
                commit: stored.header.commit,
                end: stored.header.end,
                changes: Vec::new(),
            };
            stored.for_each_change(|change| {
                batch.changes.push(change);
                Ok(())
            })?;
            events.push(Event {
                cursor: stored.header.cursor,
                batch,
            });
            Ok(())
        })?;
        Ok(events)
    }

    pub fn stats(&self) -> Result<StorageStats, Error> {
        let (batches, payload_bytes): (i64, i64) = self.db.query_row(
            "SELECT batches,bytes FROM event_usage WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let staged: i64 = self.db.query_row(
            "SELECT coalesce(sum(payload_bytes),0) FROM batches WHERE sequence IS NULL",
            [],
            |r| r.get(0),
        )?;
        let page_size: i64 = self.db.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        let pages: i64 = self.db.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let free: i64 = self
            .db
            .query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
        Ok(StorageStats {
            batches: batches as u64,
            payload_bytes: payload_bytes as u64,
            staged_payload_bytes: staged as u64,
            database_bytes: (pages * page_size) as u64,
            reusable_bytes: (free * page_size) as u64,
        })
    }
}

#[cfg(test)]
#[path = "../tests/unit/staging.rs"]
mod tests;
