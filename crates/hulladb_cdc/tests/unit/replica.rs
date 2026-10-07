use super::*;
use crate::Event;
use crate::{Batch, Column, MAX_SNAPSHOT_BYTES, Retention};
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
        source_id: Uuid::now_v7(),
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
        tables: vec![(relation(), vec![row("1", "copied")])],
    }
}
fn event(source: &Source, sequence: i64, xid: u32, commit: u64, changes: Vec<Change>) -> Event {
    Event {
        cursor: Cursor {
            source_id: source.source_id,
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
    let file = File::new();
    let source = source();
    let mut relation = relation();
    relation.columns[1].key = true;
    let rows = vec![
        row("a,b", "c"),
        row("a", "b,c"),
        row("1", "scope a"),
        row("1", "scope b"),
    ];
    let replica = Replica::initialize(
        &file.0,
        Snapshot {
            source: source.clone(),
            tables: vec![(relation.clone(), rows.clone())],
        },
    )
    .unwrap();
    let stored = replica.view(3, 100).unwrap().rows;
    assert_eq!(stored.len(), rows.len());
    for row in rows {
        assert!(stored.contains(&row));
    }
    let invalid_file = File::new();
    assert!(
        Replica::initialize(
            &invalid_file.0,
            Snapshot {
                source,
                tables: vec![(relation, vec![vec![Value::Text("1".into()), Value::Null]])],
            },
        )
        .is_err()
    );
}

#[test]
fn failed_transaction_keeps_exact_accounting_totals() {
    let file = File::new();
    let source = source();
    let replica = Replica::initialize(&file.0, snapshot(&source)).unwrap();
    let db = Connection::open(&file.0).unwrap();
    drop(replica);
    let mut replica = Replica::open(&file.0, &source).unwrap();
    let totals = || {
        db.query_row("SELECT rows,bytes FROM replica_usage", [], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })
        .unwrap()
    };
    let before = totals();
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
    assert_eq!(totals(), before);
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
    assert_eq!(totals().0, 2);
    let actual: i64 = db.query_row(
        "SELECT (SELECT sum(length(CAST(payload AS BLOB))) FROM replica_rows) + (SELECT sum(length(CAST(definition AS BLOB))) FROM replica_relations)",[],|r|r.get(0)
    ).unwrap();
    assert_eq!(totals().1, actual);
}

#[test]
fn exported_snapshot_applies_every_transaction_across_xid_wrap_and_reopen() {
    let file = File::new();
    let source = source();
    let mut replica = Replica::initialize(&file.0, snapshot(&source)).unwrap();
    for (sequence, xid) in [(1, u32::MAX - 1), (2, u32::MAX), (3, 3), (4, 4)] {
        replica
            .apply(&event(
                &source,
                sequence,
                xid,
                20 + sequence as u64 * 10,
                vec![Change::Insert {
                    relation: relation(),
                    row: row(&(sequence + 1).to_string(), "new"),
                }],
            ))
            .unwrap();
        drop(replica);
        replica = Replica::open(&file.0, &source).unwrap();
    }
    assert_eq!(replica.view(3, 100).unwrap().rows.len(), 5);
    assert_eq!(replica.cursor().unwrap().sequence, 4);
    assert!(matches!(
        replica.apply(&event(&source, 6, 5, 80, vec![])),
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
        row: row("3", &"x".repeat(MAX_SNAPSHOT_BYTES)),
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
    let db = Connection::open(&file.0).unwrap();
    db.execute_batch("CREATE TRIGGER fail_cursor BEFORE UPDATE ON replica_metadata BEGIN SELECT RAISE(ABORT,'injected'); END").unwrap();
    assert!(replica.catch_up(&log, 100).is_err());
    assert_eq!(replica.view(3, 100).unwrap().rows.len(), 1);
    db.execute_batch("DROP TRIGGER fail_cursor").unwrap();
    drop(replica);
    let mut replica = Replica::open(&file.0, &source).unwrap();
    assert_eq!(replica.catch_up(&log, 100).unwrap(), 1);
    assert_eq!(replica.view(3, 100).unwrap().rows.len(), 2);
    assert_eq!(replica.catch_up(&log, 100).unwrap(), 0);
}

#[test]
fn streaming_read_failure_rolls_back_rows_and_replica_cursor_together() {
    let file = File::new();
    let history = File::new();
    let source = source();
    let mut replica = Replica::initialize(&file.0, snapshot(&source)).unwrap();
    let before = replica.cursor().unwrap();
    let mut log = EventLog::open_with_options(
        &history.0,
        source.clone(),
        Retention::default(),
        crate::StorageOptions {
            chunk_bytes: 31,
            ..Default::default()
        },
    )
    .unwrap();
    log.append(
        &event(
            &source,
            1,
            1,
            20,
            vec![
                Change::Insert {
                    relation: relation(),
                    row: row("2", "first valid row"),
                },
                Change::Insert {
                    relation: relation(),
                    row: row("3", "second row"),
                },
            ],
        )
        .batch,
    )
    .unwrap();
    let injector = Connection::open(&history.0).unwrap();
    injector
        .execute(
            "DELETE FROM batch_chunks WHERE ordinal=(SELECT max(ordinal) FROM batch_chunks)",
            [],
        )
        .unwrap();
    assert!(replica.catch_up(&log, 1).is_err());
    assert_eq!(replica.cursor().unwrap(), before);
    assert_eq!(
        replica.view(relation().oid, 100).unwrap().rows,
        vec![row("1", "copied")]
    );
}
