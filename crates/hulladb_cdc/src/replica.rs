use std::{path::Path, time::Duration};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::{Change, Cursor, Error, EventLog, Lsn, Relation, Snapshot, Source, Value};

/// Separate durable internal materialization. Never expose it without authorization.
/// Rows retain PostgreSQL text values; this is not a general SQL/type adapter.
pub struct Replica {
    pub(crate) db: Connection,
    pub(crate) source: Source,
}

#[derive(Debug)]
pub struct ReplicaView {
    pub cursor: Cursor,
    pub rows: Vec<Vec<Value>>,
    /// False means the requested row bound cut off the result.
    pub complete: bool,
}

fn storage(path: impl AsRef<Path>) -> Result<Connection, Error> {
    let db = Connection::open(path)?;
    db.busy_timeout(Duration::from_secs(5))?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
    let mode: String = db.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    let synchronous: i64 = db.query_row("PRAGMA synchronous", [], |r| r.get(0))?;
    if mode != "wal" || synchronous != 2 {
        return Err(Error::Invalid("replica requires durable WAL/FULL mode"));
    }
    Ok(db)
}

impl Replica {
    /// Publishes the whole snapshot and cursor zero in one SQLite transaction.
    /// Refuses to replace an existing replica; recovery requires an explicit new file.
    pub fn initialize(path: impl AsRef<Path>, snapshot: Snapshot) -> Result<Self, Error> {
        let mut db = storage(path)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS replica_metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1),source TEXT NOT NULL,sequence INTEGER NOT NULL,end_lsn TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS replica_relations(oid INTEGER PRIMARY KEY,definition TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS replica_rows(relation_oid INTEGER NOT NULL,key TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(relation_oid,key));")?;
        let exists: bool =
            tx.query_row("SELECT EXISTS(SELECT 1 FROM replica_metadata)", [], |r| {
                r.get(0)
            })?;
        if exists {
            return Err(Error::Invalid("replica already initialized"));
        }
        tx.execute_batch("CREATE TABLE replica_usage(singleton INTEGER PRIMARY KEY CHECK(singleton=1),rows INTEGER NOT NULL,bytes INTEGER NOT NULL); INSERT INTO replica_usage VALUES(1,0,0);")?;
        let source = snapshot.source;
        tx.execute(
            "INSERT INTO replica_metadata VALUES(1,?1,0,?2)",
            params![serde_json::to_string(&source)?, source.start.to_string(),],
        )?;
        let mut usage = Usage::read(&tx)?;
        for (relation, rows) in snapshot.tables {
            validate_relation(&relation)?;
            let definition = serde_json::to_string(&relation)?;
            tx.execute(
                "INSERT INTO replica_relations VALUES(?1,?2)",
                params![relation.oid, definition],
            )?;
            usage.adjust(0, definition.len() as i64)?;
            for row in rows {
                insert(&tx, &relation, &row, &mut usage)?;
            }
        }
        usage.persist(&tx)?;
        tx.commit()?;
        Ok(Self { db, source })
    }

    pub fn open(path: impl AsRef<Path>, source: &Source) -> Result<Self, Error> {
        let db = storage(path)?;
        let stored: String = db.query_row(
            "SELECT source FROM replica_metadata WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if serde_json::from_str::<Source>(&stored)? != *source {
            return Err(Error::SourceMismatch);
        }
        Ok(Self {
            db,
            source: source.clone(),
        })
    }

    pub fn cursor(&self) -> Result<Cursor, Error> {
        Ok(Cursor {
            source_id: self.source.source_id,
            sequence: self.db.query_row(
                "SELECT sequence FROM replica_metadata WHERE singleton=1",
                [],
                |r| r.get(0),
            )?,
        })
    }

    /// Applies at most 100 whole retained transactions. Expiry requires a new bootstrap.
    /// Capture can acknowledge after log persistence even when materialization fails.
    pub fn catch_up(&mut self, log: &EventLog, limit: u32) -> Result<usize, Error> {
        if log.source() != &self.source {
            return Err(Error::SourceMismatch);
        }
        let mut count = 0;
        log.visit_transactions(self.cursor()?, limit, |stored| {
            self.apply_changes(
                stored.header.cursor,
                stored.header.commit,
                stored.header.end,
                |visit| stored.for_each_change(|change| visit(&change)),
            )?;
            count += 1;
            Ok(())
        })?;
        Ok(count)
    }

    #[cfg(test)]
    fn apply(&mut self, event: &crate::Event) -> Result<(), Error> {
        self.apply_changes(event.cursor, event.batch.commit, event.batch.end, |visit| {
            for change in &event.batch.changes {
                visit(change)?;
            }
            Ok(())
        })
    }

    fn apply_changes(
        &mut self,
        cursor: Cursor,
        commit: Lsn,
        end_lsn: Lsn,
        read: impl FnOnce(&mut dyn FnMut(&Change) -> Result<(), Error>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (sequence, end): (i64, String) = tx.query_row(
            "SELECT sequence,end_lsn FROM replica_metadata WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if cursor.source_id != self.source.source_id
            || sequence.checked_add(1) != Some(cursor.sequence)
        {
            return Err(Error::InvalidCursor);
        }
        if commit < end.parse::<Lsn>()? || commit >= end_lsn {
            return Err(Error::Corrupt);
        }
        let mut usage = Usage::read(&tx)?;
        // The slot's exported snapshot includes exactly the state before its
        // consistent point. Every emitted transaction must be applied, regardless
        // of the 32-bit pgoutput XID or PostgreSQL's current XID epoch.
        read(&mut |change| {
            match change {
                Change::Insert { relation, row } => {
                    check_relation(&tx, relation)?;
                    insert(&tx, relation, row, &mut usage)?;
                }
                Change::Update { relation, old, row } => {
                    check_relation(&tx, relation)?;
                    let key = key(relation, old.as_deref().unwrap_or(row))?;
                    let previous: Option<String> = tx
                        .query_row(
                            "SELECT payload FROM replica_rows WHERE relation_oid=?1 AND key=?2",
                            params![relation.oid, key],
                            |r| r.get(0),
                        )
                        .optional()?;
                    let previous = previous.ok_or(Error::Corrupt)?;
                    let prior_bytes = previous.len() as i64;
                    let previous: Vec<Value> = serde_json::from_str(&previous)?;
                    if row.len() != relation.columns.len() || previous.len() != row.len() {
                        return Err(Error::Corrupt);
                    }
                    let merged = row
                        .iter()
                        .zip(previous)
                        .map(|(new, prior)| {
                            if *new == Value::UnchangedToast {
                                prior
                            } else {
                                new.clone()
                            }
                        })
                        .collect::<Vec<_>>();
                    tx.execute(
                        "DELETE FROM replica_rows WHERE relation_oid=?1 AND key=?2",
                        params![relation.oid, key],
                    )?;
                    usage.adjust(-1, -prior_bytes)?;
                    insert(&tx, relation, &merged, &mut usage)?;
                }
                Change::Delete { relation, old } => {
                    check_relation(&tx, relation)?;
                    let removed: Option<i64> = tx.query_row(
                            "DELETE FROM replica_rows WHERE relation_oid=?1 AND key=?2 RETURNING length(CAST(payload AS BLOB))",
                            params![relation.oid, key(relation, old)?], |r|r.get(0),
                        ).optional()?;
                    usage.adjust(-1, -removed.ok_or(Error::Corrupt)?)?;
                }
                Change::Truncate { relations, .. } => {
                    for relation in relations {
                        check_relation(&tx, relation)?;
                        let (rows, bytes): (i64,i64) = tx.query_row(
                                "SELECT count(*),coalesce(sum(length(CAST(payload AS BLOB))),0) FROM replica_rows WHERE relation_oid=?1",
                                [relation.oid], |r| Ok((r.get(0)?,r.get(1)?)),
                            )?;
                        tx.execute(
                            "DELETE FROM replica_rows WHERE relation_oid=?1",
                            [relation.oid],
                        )?;
                        usage.adjust(-rows, -bytes)?;
                    }
                }
            }
            Ok(())
        })?;
        usage.persist(&tx)?;
        tx.execute(
            "UPDATE replica_metadata SET sequence=?1,end_lsn=?2 WHERE singleton=1",
            params![cursor.sequence, end_lsn.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// One SQLite read snapshot couples the row page to its materialization cursor.
    /// Ordering is internal key order, not PostgreSQL query/collation equivalence.
    pub fn view(&self, relation_oid: u32, limit: u32) -> Result<ReplicaView, Error> {
        if limit == 0 || limit > 1000 {
            return Err(Error::Invalid("invalid replica row bound"));
        }
        let tx = self.db.unchecked_transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM replica_relations WHERE oid=?1)",
            [relation_oid],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::Invalid("unknown replica relation"));
        }
        let sequence = tx.query_row(
            "SELECT sequence FROM replica_metadata WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let mut q = tx.prepare(
            "SELECT payload FROM replica_rows WHERE relation_oid=?1 ORDER BY key LIMIT ?2",
        )?;
        let values = q.query_map(params![relation_oid, limit + 1], |r| r.get::<_, String>(0))?;
        let mut rows = Vec::new();
        for value in values {
            rows.push(serde_json::from_str(&value?)?);
        }
        let complete = rows.len() <= limit as usize;
        rows.truncate(limit as usize);
        Ok(ReplicaView {
            cursor: Cursor {
                source_id: self.source.source_id,
                sequence,
            },
            rows,
            complete,
        })
    }
}

fn validate_relation(relation: &Relation) -> Result<(), Error> {
    if relation.replica_identity != b'd'
        || !relation.columns.iter().any(|c| c.key)
        || relation
            .columns
            .iter()
            .any(|c| c.key && !matches!(c.type_oid, 20 | 25 | 2950))
    {
        return Err(Error::Invalid(
            "replica requires default primary-key identity",
        ));
    }
    Ok(())
}

struct Usage {
    rows: i64,
    bytes: i64,
}
impl Usage {
    fn read(tx: &Transaction<'_>) -> Result<Self, Error> {
        Ok(tx.query_row(
            "SELECT rows,bytes FROM replica_usage WHERE singleton=1",
            [],
            |r| {
                Ok(Self {
                    rows: r.get(0)?,
                    bytes: r.get(1)?,
                })
            },
        )?)
    }
    fn adjust(&mut self, rows: i64, bytes: i64) -> Result<(), Error> {
        self.rows = self.rows.checked_add(rows).ok_or(Error::Limit)?;
        self.bytes = self.bytes.checked_add(bytes).ok_or(Error::Limit)?;
        Ok(())
    }
    fn persist(&self, tx: &Transaction<'_>) -> Result<(), Error> {
        if self.rows < 0 || self.bytes < 0 {
            return Err(Error::Corrupt);
        }
        if self.rows > crate::MAX_SNAPSHOT_ROWS as i64
            || self.bytes > crate::MAX_SNAPSHOT_BYTES as i64
        {
            return Err(Error::Limit);
        }
        tx.execute(
            "UPDATE replica_usage SET rows=?1,bytes=?2 WHERE singleton=1",
            params![self.rows, self.bytes],
        )?;
        Ok(())
    }
}

fn check_relation(tx: &Transaction<'_>, relation: &Relation) -> Result<(), Error> {
    validate_relation(relation)?;
    let stored: Option<String> = tx
        .query_row(
            "SELECT definition FROM replica_relations WHERE oid=?1",
            [relation.oid],
            |r| r.get(0),
        )
        .optional()?;
    if stored.as_deref() != Some(serde_json::to_string(relation)?.as_str()) {
        return Err(Error::SourceMismatch);
    }
    Ok(())
}

fn key(relation: &Relation, row: &[Value]) -> Result<String, Error> {
    if row.len() != relation.columns.len() {
        return Err(Error::Corrupt);
    }
    let keys: Vec<_> = relation
        .columns
        .iter()
        .zip(row)
        .filter_map(|(column, value)| column.key.then_some(value))
        .collect();
    if keys.is_empty() || keys.iter().any(|v| !matches!(v, Value::Text(_))) {
        return Err(Error::Corrupt);
    }
    // Preserve existing single-key files. A JSON tuple is collision-free even
    // when text components contain separators or JSON punctuation.
    if keys.len() == 1 {
        Ok(serde_json::to_string(keys[0])?)
    } else {
        Ok(serde_json::to_string(&keys)?)
    }
}

fn insert(
    tx: &Transaction<'_>,
    relation: &Relation,
    row: &[Value],
    usage: &mut Usage,
) -> Result<(), Error> {
    let key = key(relation, row)?;
    if row.contains(&Value::UnchangedToast) {
        return Err(Error::Corrupt);
    }
    let payload = serde_json::to_string(row)?;
    tx.execute(
        "INSERT INTO replica_rows VALUES(?1,?2,?3)",
        params![relation.oid, key, payload],
    )?;
    usage.adjust(1, payload.len() as i64)?;
    Ok(())
}

#[cfg(test)]
#[path = "../tests/unit/replica.rs"]
mod tests;
