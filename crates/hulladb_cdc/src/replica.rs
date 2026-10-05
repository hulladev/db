use std::{path::Path, time::Duration};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::{
    Change, Cursor, Error, Event, EventLog, Lsn, Relation, Snapshot, Source, Value,
    snapshot::Visibility,
};

/// Separate durable internal materialization. Never expose it without authorization.
/// Rows retain PostgreSQL text values; this is not a general SQL/type adapter.
pub struct Replica {
    db: Connection,
    source: Source,
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
        tx.execute_batch("CREATE TABLE IF NOT EXISTS replica_metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1),format INTEGER NOT NULL,source TEXT NOT NULL,visibility TEXT NOT NULL,through_lsn TEXT NOT NULL,sequence INTEGER NOT NULL,end_lsn TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS replica_relations(oid INTEGER PRIMARY KEY,definition TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS replica_rows(relation_oid INTEGER NOT NULL,key TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(relation_oid,key));")?;
        let exists: bool =
            tx.query_row("SELECT EXISTS(SELECT 1 FROM replica_metadata)", [], |r| {
                r.get(0)
            })?;
        if exists {
            return Err(Error::Invalid("replica already initialized"));
        }
        install_usage(&tx)?;
        let source = snapshot.source;
        tx.execute(
            "INSERT INTO replica_metadata VALUES(1,1,?1,?2,?3,0,?4)",
            params![
                serde_json::to_string(&source)?,
                serde_json::to_string(&snapshot.visibility)?,
                snapshot.through.to_string(),
                source.start.to_string(),
            ],
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
        let (format, stored): (i64, String) = db.query_row(
            "SELECT format,source FROM replica_metadata WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if format != 1 {
            return Err(Error::Corrupt);
        }
        if serde_json::from_str::<Source>(&stored)? != *source {
            return Err(Error::SourceMismatch);
        }
        // Upgrade accounting in existing files before applying another event.
        // The initial aggregate runs once; each transaction saves its size delta.
        let tx = db.unchecked_transaction()?;
        install_usage(&tx)?;
        tx.commit()?;
        Ok(Self {
            db,
            source: source.clone(),
        })
    }

    pub fn cursor(&self) -> Result<Cursor, Error> {
        Ok(Cursor {
            generation: self.source.generation,
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
        let events = log.since(self.cursor()?, limit)?;
        for event in &events {
            self.apply(event)?;
        }
        Ok(events.len())
    }

    fn apply(&mut self, event: &Event) -> Result<(), Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (sequence, end, visibility, through): (i64,String,String,String) = tx.query_row(
            "SELECT sequence,end_lsn,visibility,through_lsn FROM replica_metadata WHERE singleton=1", [],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
        )?;
        if event.cursor.generation != self.source.generation
            || sequence.checked_add(1) != Some(event.cursor.sequence)
        {
            return Err(Error::InvalidCursor);
        }
        if event.batch.commit < end.parse::<Lsn>()? || event.batch.commit >= event.batch.end {
            return Err(Error::Corrupt);
        }
        let mut usage = Usage::read(&tx)?;
        let visibility: Visibility = serde_json::from_str(&visibility)?;
        let already_copied =
            event.batch.commit < through.parse::<Lsn>()? && visibility.visible(event.batch.xid)?;
        // Commit positions after the fence cannot already be present in the copy.
        // This also makes future XID wrap irrelevant to the completed bootstrap.
        if !already_copied {
            for change in &event.batch.changes {
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
            }
        }
        usage.persist(&tx)?;
        tx.execute(
            "UPDATE replica_metadata SET sequence=?1,end_lsn=?2 WHERE singleton=1",
            params![event.cursor.sequence, event.batch.end.to_string()],
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
                generation: self.source.generation,
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

fn install_usage(db: &Connection) -> Result<(), Error> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS replica_usage(singleton INTEGER PRIMARY KEY CHECK(singleton=1),rows INTEGER NOT NULL,bytes INTEGER NOT NULL);
        INSERT INTO replica_usage SELECT 1,
          (SELECT count(*) FROM replica_rows),
          (SELECT coalesce(sum(length(CAST(payload AS BLOB))),0) FROM replica_rows) +
          (SELECT coalesce(sum(length(CAST(definition AS BLOB))),0) FROM replica_relations)
          WHERE NOT EXISTS(SELECT 1 FROM replica_usage);")?;
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
mod tests {
    use super::*;
    use crate::{Batch, Column, Retention};
    use uuid::Uuid;

    struct File(std::path::PathBuf);
    impl File {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("hulladb-replica-{}.sqlite", Uuid::now_v7())))
        }
    }
    impl Drop for File {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
            }
        }
    }
    fn source() -> Source {
        Source {
            generation: Uuid::now_v7(),
            system_id: "fixture".into(),
            database_oid: 1,
            publication_oid: 2,
            slot: "slot".into(),
            publication: "pub".into(),
            start: Lsn(10),
        }
    }
    fn relation() -> Relation {
        Relation {
            oid: 3,
            namespace: "public".into(),
            name: "tasks".into(),
            replica_identity: b'd',
            columns: vec![
                Column {
                    name: "id".into(),
                    type_oid: 20,
                    type_modifier: -1,
                    key: true,
                },
                Column {
                    name: "title".into(),
                    type_oid: 25,
                    type_modifier: -1,
                    key: false,
                },
            ],
        }
    }
    fn row(id: &str, text: &str) -> Vec<Value> {
        vec![Value::Text(id.into()), Value::Text(text.into())]
    }
    fn snapshot(source: &Source) -> Snapshot {
        Snapshot {
            source: source.clone(),
            visibility: Visibility::parse("10:20:10,14").unwrap(),
            through: Lsn(100),
            tables: vec![(relation(), vec![row("1", "copied")])],
        }
    }
    fn event(source: &Source, sequence: i64, xid: u32, commit: u64, changes: Vec<Change>) -> Event {
        Event {
            cursor: Cursor {
                generation: source.generation,
                sequence,
            },
            batch: Batch {
                xid,
                commit: Lsn(commit),
                end: Lsn(commit + 1),
                changes,
            },
        }
    }

    #[test]
    fn composite_keys_preserve_scope_and_unambiguous_components() {
        let mut relation = relation();
        relation.columns[1].key = true;
        validate_relation(&relation).unwrap();
        assert_ne!(
            key(&relation, &row("a,b", "c")).unwrap(),
            key(&relation, &row("a", "b,c")).unwrap()
        );
        assert_ne!(
            key(&relation, &row("1", "scope a")).unwrap(),
            key(&relation, &row("1", "scope b")).unwrap()
        );
        assert!(key(&relation, &[Value::Text("1".into()), Value::Null]).is_err());
    }

    #[test]
    fn accounting_upgrade_and_failed_transaction_keep_exact_totals() {
        let file = File::new();
        let source = source();
        let replica = Replica::initialize(&file.0, snapshot(&source)).unwrap();
        replica
            .db
            .execute_batch("DROP TABLE replica_usage;")
            .unwrap();
        drop(replica);
        let mut replica = Replica::open(&file.0, &source).unwrap();
        let totals = |replica: &Replica| {
            replica
                .db
                .query_row("SELECT rows,bytes FROM replica_usage", [], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                })
                .unwrap()
        };
        let before = totals(&replica);
        assert_eq!(before.0, 1);
        let changes = vec![
            Change::Insert {
                relation: relation(),
                row: row("2", "new"),
            },
            Change::Delete {
                relation: relation(),
                old: row("missing", ""),
            },
        ];
        assert!(replica.apply(&event(&source, 1, 21, 100, changes)).is_err());
        assert_eq!(totals(&replica), before);
        replica
            .apply(&event(
                &source,
                1,
                21,
                100,
                vec![Change::Insert {
                    relation: relation(),
                    row: row("2", "new"),
                }],
            ))
            .unwrap();
        assert_eq!(totals(&replica).0, 2);
        let actual: i64 = replica.db.query_row(
            "SELECT (SELECT sum(length(CAST(payload AS BLOB))) FROM replica_rows) + (SELECT sum(length(CAST(definition AS BLOB))) FROM replica_relations)",[],|r|r.get(0)
        ).unwrap();
        assert_eq!(totals(&replica).1, actual);
    }

    #[test]
    fn visibility_fence_handles_commits_during_snapshot_acquisition() {
        let file = File::new();
        let source = source();
        let mut replica = Replica::initialize(&file.0, snapshot(&source)).unwrap();
        replica
            .apply(&event(
                &source,
                1,
                11,
                20,
                vec![Change::Insert {
                    relation: relation(),
                    row: row("1", "copied"),
                }],
            ))
            .unwrap();
        replica
            .apply(&event(
                &source,
                2,
                14,
                30,
                vec![Change::Insert {
                    relation: relation(),
                    row: row("2", "active"),
                }],
            ))
            .unwrap();
        replica
            .apply(&event(
                &source,
                3,
                20,
                40,
                vec![Change::Insert {
                    relation: relation(),
                    row: row("3", "new"),
                }],
            ))
            .unwrap();
        // After the fence, ignore XID visibility altogether, including wrap.
        replica
            .apply(&event(
                &source,
                4,
                3,
                100,
                vec![Change::Insert {
                    relation: relation(),
                    row: row("4", "wrapped"),
                }],
            ))
            .unwrap();
        assert_eq!(replica.view(3, 100).unwrap().rows.len(), 4);
        assert_eq!(replica.cursor().unwrap().sequence, 4);
        assert!(matches!(
            replica.apply(&event(&source, 6, 21, 110, vec![])),
            Err(Error::InvalidCursor)
        ));
        assert_eq!(replica.cursor().unwrap().sequence, 4);
    }

    #[test]
    fn failed_bootstrap_never_publishes_partial_state_or_overwrites_a_replica() {
        let file = File::new();
        let source = source();
        let mut invalid = snapshot(&source);
        invalid.tables[0].1.push(row("1", "duplicate"));
        assert!(Replica::initialize(&file.0, invalid).is_err());
        assert!(Replica::open(&file.0, &source).is_err());
        let replica = Replica::initialize(&file.0, snapshot(&source)).unwrap();
        assert!(Replica::initialize(&file.0, snapshot(&source)).is_err());
        assert_eq!(replica.view(3, 100).unwrap().rows, vec![row("1", "copied")]);
        assert!(Replica::initialize(":memory:", snapshot(&source)).is_err());
    }

    #[test]
    fn malformed_batch_schema_and_materialization_limits_roll_back() {
        let file = File::new();
        let source = source();
        let mut replica = Replica::initialize(&file.0, snapshot(&source)).unwrap();
        let insert = Change::Insert {
            relation: relation(),
            row: row("2", "valid"),
        };
        let missing = Change::Delete {
            relation: relation(),
            old: row("missing", ""),
        };
        assert!(matches!(
            replica.apply(&event(&source, 1, 21, 100, vec![insert.clone(), missing])),
            Err(Error::Corrupt)
        ));
        assert_eq!(replica.view(3, 100).unwrap().rows, vec![row("1", "copied")]);
        assert_eq!(replica.cursor().unwrap().sequence, 0);
        let mut changed = relation();
        changed.columns[1].type_modifier = 42;
        assert!(matches!(
            replica.apply(&event(
                &source,
                1,
                21,
                100,
                vec![Change::Insert {
                    relation: changed,
                    row: row("2", "changed")
                }]
            )),
            Err(Error::SourceMismatch)
        ));
        let large = Change::Insert {
            relation: relation(),
            row: row("3", &"x".repeat(crate::MAX_SNAPSHOT_BYTES)),
        };
        assert!(matches!(
            replica.apply(&event(&source, 1, 21, 100, vec![insert, large])),
            Err(Error::Limit)
        ));
        assert_eq!(replica.cursor().unwrap().sequence, 0);
        assert_eq!(replica.view(3, 100).unwrap().rows.len(), 1);
    }

    #[test]
    fn log_recovery_after_cursor_commit_failure_does_not_duplicate_rows() {
        let file = File::new();
        let log_file = File::new();
        let source = source();
        let mut replica = Replica::initialize(&file.0, snapshot(&source)).unwrap();
        let mut log = EventLog::open(&log_file.0, source.clone(), Retention::default()).unwrap();
        let event = event(
            &source,
            1,
            21,
            100,
            vec![Change::Insert {
                relation: relation(),
                row: row("2", "new"),
            }],
        );
        log.append(&event.batch).unwrap();
        replica.db.execute_batch("CREATE TRIGGER fail_cursor BEFORE UPDATE ON replica_metadata BEGIN SELECT RAISE(ABORT,'injected'); END").unwrap();
        assert!(replica.catch_up(&log, 100).is_err());
        assert_eq!(replica.view(3, 100).unwrap().rows.len(), 1);
        replica
            .db
            .execute_batch("DROP TRIGGER fail_cursor")
            .unwrap();
        drop(replica);
        let mut replica = Replica::open(&file.0, &source).unwrap();
        assert_eq!(replica.catch_up(&log, 100).unwrap(), 1);
        assert_eq!(replica.view(3, 100).unwrap().rows.len(), 2);
        assert_eq!(replica.catch_up(&log, 100).unwrap(), 0);
    }
}
