use std::{path::Path, time::Duration};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{Batch, Cursor, Error, Event, Lsn, MAX_TRANSACTION_BYTES, Source};

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

#[derive(Debug, Clone, Copy)]
pub struct StorageStats {
    pub batches: u64,
    pub payload_bytes: u64,
    pub database_bytes: u64,
    pub reusable_bytes: u64,
}

/// One internal durable tail log. Reads require authorization at a later service boundary.
pub struct EventLog {
    db: Connection,
    source: Source,
    retention: Retention,
}

impl EventLog {
    pub fn open(
        path: impl AsRef<Path>,
        source: Source,
        retention: Retention,
    ) -> Result<Self, Error> {
        if retention.max_batches == 0
            || retention.max_batches > 100_000
            || retention.max_bytes < MAX_TRANSACTION_BYTES as u64
            || retention.max_bytes > i64::MAX as u64
        {
            return Err(Error::Invalid("invalid retention bounds"));
        }
        let mut db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(5))?;
        // FULL commits flush the SQLite WAL before PostgreSQL may be acknowledged.
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
        let mode: String = db.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
        let synchronous: i64 = db.query_row("PRAGMA synchronous", [], |r| r.get(0))?;
        if mode != "wal" || synchronous != 2 {
            return Err(Error::Invalid(
                "event storage requires durable WAL/FULL mode",
            ));
        }
        db.execute_batch("CREATE TABLE IF NOT EXISTS metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1), format INTEGER NOT NULL, source TEXT NOT NULL, checkpoint TEXT NOT NULL, head INTEGER NOT NULL, floor INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS batches(sequence INTEGER PRIMARY KEY, end_lsn TEXT NOT NULL UNIQUE, payload TEXT NOT NULL);")?;
        let encoded = serde_json::to_string(&source)?;
        db.execute(
            "INSERT OR IGNORE INTO metadata VALUES(1,1,?1,?2,0,0)",
            params![encoded, source.start.to_string()],
        )?;
        let (format, existing): (i64, String) = db.query_row(
            "SELECT format,source FROM metadata WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if !matches!(format, 1 | 2) {
            return Err(Error::Corrupt);
        }
        if serde_json::from_str::<Source>(&existing)? != source {
            return Err(Error::SourceMismatch);
        }
        // Upgrade once under the same write lock used by append. Format two
        // prevents older collectors from opening storage they cannot account for.
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let format: i64 =
            tx.query_row("SELECT format FROM metadata WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
        if format == 1 {
            tx.execute_batch("CREATE TABLE event_usage(singleton INTEGER PRIMARY KEY CHECK(singleton=1), batches INTEGER NOT NULL CHECK(batches>=0), bytes INTEGER NOT NULL CHECK(bytes>=0));
                INSERT INTO event_usage SELECT 1,count(*),coalesce(sum(length(CAST(payload AS BLOB))),0) FROM batches;
                UPDATE metadata SET format=2 WHERE singleton=1;")?;
        } else if format != 2 {
            return Err(Error::Corrupt);
        }
        // Also fail immediately if a current-format log lost its accounting row.
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

    /// Record a fully scanned WAL boundary with no published transaction output.
    /// Only the capture owner may call this at a decoded boundary outside a transaction.
    pub(crate) fn scanned_through(&mut self, position: Lsn) -> Result<(), Error> {
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

    pub fn head(&self) -> Result<Cursor, Error> {
        let sequence =
            self.db
                .query_row("SELECT head FROM metadata WHERE singleton=1", [], |r| {
                    r.get(0)
                })?;
        Ok(Cursor {
            generation: self.source.generation,
            sequence,
        })
    }

    /// Commit one complete transaction and its checkpoint atomically. Return None on replay.
    pub fn append(&mut self, batch: &Batch) -> Result<Option<Cursor>, Error> {
        if batch.commit >= batch.end
            || batch.commit < self.source.start
            || batch.changes.len() > crate::MAX_TRANSACTION_CHANGES
        {
            return Err(Error::Invalid("invalid transaction boundary"));
        }
        let payload = encode(batch)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (checkpoint, head, mut count, mut bytes): (String, i64, i64, i64) = tx.query_row(
            "SELECT checkpoint,head,event_usage.batches,event_usage.bytes FROM metadata
             JOIN event_usage ON event_usage.singleton=metadata.singleton WHERE metadata.singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let checkpoint: Lsn = checkpoint.parse()?;
        if batch.end <= checkpoint {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT payload FROM batches WHERE end_lsn=?1",
                    [batch.end.to_string()],
                    |r| r.get(0),
                )
                .optional()?;
            if existing.is_some_and(|value| value != payload) {
                return Err(Error::Corrupt);
            }
            return Ok(None);
        }
        if batch.commit < checkpoint {
            return Err(Error::Corrupt);
        }
        let sequence = head.checked_add(1).ok_or(Error::Corrupt)?;
        tx.execute(
            "INSERT INTO batches VALUES(?1,?2,?3)",
            params![sequence, batch.end.to_string(), payload],
        )?;
        tx.execute(
            "UPDATE metadata SET checkpoint=?1,head=?2 WHERE singleton=1",
            params![batch.end.to_string(), sequence],
        )?;
        count = count.checked_add(1).ok_or(Error::Limit)?;
        bytes = bytes
            .checked_add(payload.len() as i64)
            .ok_or(Error::Limit)?;
        let mut floor = None;
        while count > i64::from(self.retention.max_batches)
            || bytes > self.retention.max_bytes as i64
        {
            let (oldest, removed_bytes): (i64, i64) = tx.query_row(
                "DELETE FROM batches WHERE sequence=(SELECT min(sequence) FROM batches)
                 RETURNING sequence,length(CAST(payload AS BLOB))",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            count = count
                .checked_sub(1)
                .filter(|value| *value >= 0)
                .ok_or(Error::Corrupt)?;
            bytes = bytes
                .checked_sub(removed_bytes)
                .filter(|value| *value >= 0)
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
        Ok(Some(Cursor {
            generation: self.source.generation,
            sequence,
        }))
    }

    pub fn since(&self, cursor: Cursor, limit: u32) -> Result<Vec<Event>, Error> {
        if limit == 0 || limit > 100 {
            return Err(Error::Invalid("invalid page limit"));
        }
        // One snapshot protects the retention watermark/read pair from concurrent maintenance.
        let tx = self.db.unchecked_transaction()?;
        let (floor, head): (i64, i64) = tx.query_row(
            "SELECT floor,head FROM metadata WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if cursor.generation != self.source.generation
            || cursor.sequence < 0
            || cursor.sequence > head
        {
            return Err(Error::InvalidCursor);
        }
        if cursor.sequence < floor {
            return Err(Error::CursorExpired);
        }
        let mut q = tx.prepare(
            "SELECT sequence,payload FROM batches WHERE sequence>?1 ORDER BY sequence LIMIT ?2",
        )?;
        let mut rows = q.query(params![cursor.sequence, limit])?;
        let mut events = Vec::new();
        let mut previous = cursor.sequence;
        while let Some(row) = rows.next()? {
            let sequence: i64 = row.get(0)?;
            if previous.checked_add(1) != Some(sequence) {
                return Err(Error::Corrupt);
            }
            let payload: String = row.get(1)?;
            events.push(Event {
                cursor: Cursor {
                    generation: self.source.generation,
                    sequence,
                },
                batch: serde_json::from_str(&payload)?,
            });
            previous = sequence;
        }
        Ok(events)
    }

    pub fn stats(&self) -> Result<StorageStats, Error> {
        let (batches, payload_bytes): (i64, i64) = self.db.query_row(
            "SELECT batches,bytes FROM event_usage WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let page_size: i64 = self.db.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        let pages: i64 = self.db.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let free: i64 = self
            .db
            .query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
        Ok(StorageStats {
            batches: batches as u64,
            payload_bytes: payload_bytes as u64,
            database_bytes: (pages * page_size) as u64,
            reusable_bytes: (free * page_size) as u64,
        })
    }
}

// Bound JSON escaping/metadata expansion during serialization, before a large
// temporary payload can be allocated. A transaction is never partially stored.
fn encode(batch: &Batch) -> Result<String, Error> {
    struct Bounded {
        bytes: Vec<u8>,
        exceeded: bool,
    }
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_TRANSACTION_BYTES - self.bytes.len() {
                self.exceeded = true;
                return Err(std::io::Error::other("transaction exceeds byte limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded {
        bytes: Vec::new(),
        exceeded: false,
    };
    let result = serde_json::to_writer(&mut writer, batch);
    if writer.exceeded {
        return Err(Error::Limit);
    }
    result?;
    String::from_utf8(writer.bytes).map_err(|_| Error::Corrupt)
}
