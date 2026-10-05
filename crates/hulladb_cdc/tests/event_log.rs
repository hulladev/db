use std::path::PathBuf;

use hulladb_cdc::{
    Batch, Change, Column, Cursor, Error, EventLog, Lsn, Relation, Retention, Source, Value,
};
use uuid::Uuid;

fn source() -> Source {
    Source {
        generation: Uuid::now_v7(),
        system_id: "fixture".into(),
        database_oid: 1,
        publication_oid: 2,
        slot: "test".into(),
        publication: "test".into(),
        start: Lsn(10),
    }
}

fn batch(position: u64, text: &str) -> Batch {
    Batch {
        xid: position as u32,
        commit: Lsn(position),
        end: Lsn(position + 1),
        changes: vec![Change::Insert {
            relation: Relation {
                oid: 3,
                namespace: "public".into(),
                name: "tasks".into(),
                replica_identity: b'd',
                columns: vec![Column {
                    name: "title".into(),
                    type_oid: 25,
                    type_modifier: -1,
                    key: true,
                }],
            },
            row: vec![Value::Text(text.into())],
        }],
    }
}

struct File(PathBuf);
impl File {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("hulladb-cdc-{}.sqlite", Uuid::now_v7())))
    }
}
impl Drop for File {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

#[test]
fn durable_reopen_replay_and_source_identity() {
    let file = File::new();
    let source = source();
    let mut log = EventLog::open(&file.0, source.clone(), Retention::default()).unwrap();
    let initial = log.head().unwrap();
    let first = batch(20, "hello");
    let cursor = log.append(&first).unwrap().unwrap();
    assert_eq!(cursor.sequence, 1);
    assert_eq!(log.checkpoint().unwrap(), first.end);
    assert!(log.append(&first).unwrap().is_none());
    let mut contradictory = first.clone();
    contradictory.xid += 1;
    assert!(matches!(log.append(&contradictory), Err(Error::Corrupt)));
    drop(log);
    let log = EventLog::open(&file.0, source.clone(), Retention::default()).unwrap();
    assert_eq!(log.since(initial, 100).unwrap()[0].batch, first);
    assert_eq!(log.head().unwrap(), cursor);
    assert_eq!(log.checkpoint().unwrap(), Lsn(21));
    let mut other = source.clone();
    other.generation = Uuid::now_v7();
    assert!(matches!(
        EventLog::open(&file.0, other, Retention::default()),
        Err(Error::SourceMismatch)
    ));
}

#[test]
fn retention_expiration_scope_and_page_boundaries() {
    let file = File::new();
    let source = source();
    let mut log = EventLog::open(
        &file.0,
        source.clone(),
        Retention {
            max_batches: 2,
            ..Retention::default()
        },
    )
    .unwrap();
    let initial = log.head().unwrap();
    let first = log.append(&batch(20, "first")).unwrap().unwrap();
    log.append(&batch(30, "second")).unwrap();
    let head = log.append(&batch(40, "third")).unwrap().unwrap();
    assert!(matches!(log.since(initial, 100), Err(Error::CursorExpired)));
    let page = log.since(first, 1).unwrap();
    assert_eq!(page[0].cursor.sequence, 2);
    assert_eq!(log.since(page[0].cursor, 100).unwrap()[0].cursor, head);
    assert!(log.since(head, 100).unwrap().is_empty());
    assert_eq!(log.stats().unwrap().batches, 2);
    for cursor in [
        Cursor {
            generation: Uuid::now_v7(),
            sequence: 1,
        },
        Cursor {
            sequence: -1,
            ..initial
        },
        Cursor {
            sequence: 4,
            ..initial
        },
    ] {
        assert!(matches!(log.since(cursor, 100), Err(Error::InvalidCursor)));
    }
    assert!(log.since(head, 0).is_err());
    assert!(log.since(head, 101).is_err());
}

#[test]
fn byte_retention_and_transaction_limits() {
    let file = File::new();
    let mut log = EventLog::open(
        &file.0,
        source(),
        Retention {
            max_batches: 100,
            max_bytes: hulladb_cdc::MAX_TRANSACTION_BYTES as u64,
        },
    )
    .unwrap();
    let text = "a".repeat(2_500_000);
    log.append(&batch(20, &text)).unwrap();
    log.append(&batch(30, &text)).unwrap();
    let stats = log.stats().unwrap();
    assert_eq!(stats.batches, 1);
    assert!(stats.payload_bytes <= hulladb_cdc::MAX_TRANSACTION_BYTES as u64);
    let before = log.head().unwrap();
    assert!(matches!(
        log.append(&batch(40, &"a".repeat(hulladb_cdc::MAX_TRANSACTION_BYTES))),
        Err(Error::Limit)
    ));
    assert_eq!(log.head().unwrap(), before);
    assert_eq!(log.checkpoint().unwrap(), Lsn(31));
}

#[test]
fn volatile_storage_is_rejected_before_capture_can_acknowledge_it() {
    assert!(matches!(
        EventLog::open(":memory:", source(), Retention::default()),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn failed_sqlite_commit_keeps_batch_and_checkpoint_together() {
    let file = File::new();
    let mut log = EventLog::open(&file.0, source(), Retention::default()).unwrap();
    let injector = rusqlite::Connection::open(&file.0).unwrap();
    injector.execute_batch("CREATE TRIGGER fail_metadata BEFORE UPDATE ON metadata BEGIN SELECT RAISE(ABORT,'injected checkpoint failure'); END;").unwrap();
    assert!(log.append(&batch(20, "failed")).is_err());
    assert_eq!(log.checkpoint().unwrap(), Lsn(10));
    assert_eq!(log.head().unwrap().sequence, 0);
    assert_eq!(log.stats().unwrap().batches, 0);
    injector
        .execute_batch("DROP TRIGGER fail_metadata")
        .unwrap();
    assert_eq!(
        log.append(&batch(20, "retry")).unwrap().unwrap().sequence,
        1
    );
}

#[test]
fn lsn_roundtrip_and_malformed_values() {
    for position in [0, 10, u64::MAX, 0xffff_ffff, 0x1234_0000_9876] {
        let lsn = Lsn(position);
        assert_eq!(lsn.to_string().parse::<Lsn>().unwrap(), lsn);
    }
    for value in ["", "1", "x/1", "0/100000000", "1/1/1"] {
        assert!(value.parse::<Lsn>().is_err());
    }
}

#[test]
fn legacy_log_upgrade_preserves_history_and_accounts_for_multi_batch_pruning() {
    let file = File::new();
    let source = source();
    let mut log = EventLog::open(&file.0, source.clone(), Retention::default()).unwrap();
    let initial = log.head().unwrap();
    for position in [20, 30, 40, 50] {
        log.append(&batch(position, "žluťoučký 🦀")).unwrap();
    }
    let head = log.head().unwrap();
    let checkpoint = log.checkpoint().unwrap();
    let stats = log.stats().unwrap();
    drop(log);
    let db = rusqlite::Connection::open(&file.0).unwrap();
    // Recreate the pre-accounting format with real retained payloads.
    db.execute_batch("DROP TABLE event_usage; UPDATE metadata SET format=1;
        CREATE TRIGGER fail_upgrade BEFORE UPDATE ON metadata BEGIN SELECT RAISE(ABORT,'injected upgrade failure'); END;").unwrap();
    assert!(EventLog::open(&file.0, source.clone(), Retention::default()).is_err());
    assert_eq!(
        db.query_row("SELECT format FROM metadata", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(
        !db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='event_usage')",
            [],
            |r| r.get::<_, bool>(0)
        )
        .unwrap()
    );
    db.execute_batch("DROP TRIGGER fail_upgrade").unwrap();
    let mut log = EventLog::open(
        &file.0,
        source.clone(),
        Retention {
            max_batches: 1,
            ..Retention::default()
        },
    )
    .unwrap();
    assert_eq!(log.head().unwrap(), head);
    assert_eq!(log.checkpoint().unwrap(), checkpoint);
    assert_eq!(log.since(initial, 100).unwrap().len(), 4);
    assert_eq!(log.stats().unwrap().batches, stats.batches);
    assert_eq!(log.stats().unwrap().payload_bytes, stats.payload_bytes);
    assert!(log.append(&batch(50, "žluťoučký 🦀")).unwrap().is_none());
    assert_eq!(log.stats().unwrap().payload_bytes, stats.payload_bytes);
    log.append(&batch(60, "new 🦀")).unwrap();
    assert!(matches!(log.since(initial, 100), Err(Error::CursorExpired)));
    assert_eq!(log.since(head, 100).unwrap().len(), 1);
    let actual: (i64, i64) = db
        .query_row(
            "SELECT count(*),sum(length(CAST(payload AS BLOB))) FROM batches",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(log.stats().unwrap().batches, actual.0 as u64);
    assert_eq!(log.stats().unwrap().payload_bytes, actual.1 as u64);
    drop(log);
    let log = EventLog::open(&file.0, source, Retention::default()).unwrap();
    assert_eq!(log.stats().unwrap().payload_bytes, actual.1 as u64);
    assert_eq!(
        db.query_row("SELECT format FROM metadata", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn accounting_failure_rolls_back_eviction_checkpoint_and_usage_before_retry() {
    let file = File::new();
    let source = source();
    let retention = Retention {
        max_batches: 2,
        ..Retention::default()
    };
    let mut log = EventLog::open(&file.0, source.clone(), retention).unwrap();
    let initial = log.head().unwrap();
    let first = log.append(&batch(20, "first 🦀")).unwrap().unwrap();
    let second = log.append(&batch(30, "second")).unwrap().unwrap();
    let stats = log.stats().unwrap();
    let injector = rusqlite::Connection::open(&file.0).unwrap();
    injector.execute_batch("CREATE TRIGGER fail_usage BEFORE UPDATE ON event_usage BEGIN SELECT RAISE(ABORT,'injected accounting failure'); END").unwrap();
    assert!(log.append(&batch(40, "third")).is_err());
    assert_eq!(log.head().unwrap(), second);
    assert_eq!(log.checkpoint().unwrap(), Lsn(31));
    assert_eq!(log.since(initial, 100).unwrap().len(), 2);
    assert_eq!(log.stats().unwrap().batches, stats.batches);
    assert_eq!(log.stats().unwrap().payload_bytes, stats.payload_bytes);
    drop(log);
    let mut log = EventLog::open(&file.0, source, retention).unwrap();
    assert_eq!(log.since(initial, 100).unwrap().len(), 2);
    injector.execute_batch("DROP TRIGGER fail_usage").unwrap();
    assert_eq!(
        log.append(&batch(40, "third")).unwrap().unwrap().sequence,
        3
    );
    assert!(matches!(log.since(initial, 100), Err(Error::CursorExpired)));
    assert_eq!(log.since(first, 100).unwrap().len(), 2);
    let actual: i64 = injector
        .query_row(
            "SELECT sum(length(CAST(payload AS BLOB))) FROM batches",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(log.stats().unwrap().batches, 2);
    assert_eq!(log.stats().unwrap().payload_bytes, actual as u64);
}
