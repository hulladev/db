use super::*;
use crate::{Column, Relation, Value};
use uuid::Uuid;

struct File(std::path::PathBuf);
impl File {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("hulladb-stage-{}.sqlite", Uuid::now_v7())))
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
        slot: "fixture".into(),
        publication: "fixture".into(),
        start: Lsn(10),
    }
}
fn change(title: &str) -> Change {
    Change::Insert {
        relation: Relation {
            oid: 1,
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
        row: vec![Value::Text(title.into())],
    }
}
fn options() -> StorageOptions {
    StorageOptions {
        chunk_bytes: 31,
        ..StorageOptions::default()
    }
}

#[test]
fn durable_chunks_are_hidden_until_commit_and_replay_survives_different_chunk_sizes() {
    let file = File::new();
    let source = source();
    let mut log =
        EventLog::open_with_options(&file.0, source.clone(), Retention::default(), options())
            .unwrap();
    let initial = log.head().unwrap();
    log.begin_transaction(1, Lsn(20)).unwrap();
    let row = change("žluťoučký 🦀\n\"escaped\"");
    for _ in 0..20 {
        log.push_change(&row).unwrap();
    }
    let observer = EventLog::open(&file.0, source.clone(), Retention::default()).unwrap();
    assert!(observer.stats().unwrap().staged_payload_bytes > 0);
    assert!(observer.since(initial, 100).unwrap().is_empty());
    assert_eq!(observer.checkpoint().unwrap(), source.start);
    let db = Connection::open(&file.0).unwrap();
    let largest: i64 = db
        .query_row("SELECT max(length(payload)) FROM batch_chunks", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(largest <= options().chunk_bytes as i64);
    let cursor = log.finish_transaction(Lsn(20), Lsn(21)).unwrap().0.unwrap();
    let events = observer.since(initial, 1).unwrap();
    assert_eq!(events[0].batch.changes, vec![row.clone(); 20]);
    drop(log);
    let mut log = EventLog::open_with_options(
        &file.0,
        source,
        Retention::default(),
        StorageOptions {
            chunk_bytes: 97,
            ..options()
        },
    )
    .unwrap();
    log.begin_transaction(1, Lsn(20)).unwrap();
    for _ in 0..20 {
        log.push_change(&row).unwrap();
    }
    assert_eq!(
        log.finish_transaction(Lsn(20), Lsn(21)).unwrap(),
        (None, true)
    );
    assert_eq!(log.head().unwrap(), cursor);
    assert_eq!(log.stats().unwrap().staged_payload_bytes, 0);
}

#[test]
fn restart_discards_unpublished_chunks_and_commit_failure_cannot_publish_a_prefix() {
    let file = File::new();
    let source = source();
    let mut log =
        EventLog::open_with_options(&file.0, source.clone(), Retention::default(), options())
            .unwrap();
    log.begin_transaction(1, Lsn(20)).unwrap();
    log.push_change(&change("unfinished")).unwrap();
    assert!(log.stats().unwrap().staged_payload_bytes > 0);
    drop(log); // The durable orphan mimics termination before the upstream COMMIT.
    let mut log =
        EventLog::open_with_options(&file.0, source.clone(), Retention::default(), options())
            .unwrap();
    assert_eq!(log.head().unwrap().sequence, 0);
    assert!(log.stats().unwrap().staged_payload_bytes > 0);
    log.begin_transaction(1, Lsn(20)).unwrap();
    log.push_change(&change("replayed")).unwrap();
    let injector = Connection::open(&file.0).unwrap();
    injector.execute_batch("CREATE TRIGGER fail_commit BEFORE UPDATE ON metadata BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(log.finish_transaction(Lsn(20), Lsn(21)).is_err());
    assert_eq!(log.head().unwrap().sequence, 0);
    assert_eq!(log.checkpoint().unwrap(), source.start);
    assert_eq!(log.stats().unwrap().batches, 0);
    assert_eq!(log.stats().unwrap().staged_payload_bytes, 0);
    injector.execute_batch("DROP TRIGGER fail_commit").unwrap();
    log.begin_transaction(1, Lsn(20)).unwrap();
    log.push_change(&change("replayed")).unwrap();
    log.finish_transaction(Lsn(20), Lsn(21)).unwrap();
    assert_eq!(log.head().unwrap().sequence, 1);
    assert_eq!(log.stats().unwrap().batches, 1);
}

#[test]
fn explicit_staging_quota_fails_without_acknowledgement_and_can_be_raised_for_retry() {
    let file = File::new();
    let source = source();
    let mut log = EventLog::open_with_options(
        &file.0,
        source.clone(),
        Retention::default(),
        StorageOptions {
            max_staged_payload_bytes: Some(500),
            ..options()
        },
    )
    .unwrap();
    log.begin_transaction(1, Lsn(20)).unwrap();
    log.push_change(&change("first")).unwrap();
    assert!(matches!(
        log.push_change(&change(&"x".repeat(1000))),
        Err(Error::StagingBudget { .. })
    ));
    assert!(log.finish_transaction(Lsn(20), Lsn(21)).is_err());
    assert_eq!(log.head().unwrap().sequence, 0);
    assert_eq!(log.checkpoint().unwrap(), source.start);
    drop(log);
    let mut log =
        EventLog::open_with_options(&file.0, source, Retention::default(), options()).unwrap();
    log.begin_transaction(1, Lsn(20)).unwrap();
    log.push_change(&change("first")).unwrap();
    log.push_change(&change(&"x".repeat(1000))).unwrap();
    log.finish_transaction(Lsn(20), Lsn(21)).unwrap();
    assert_eq!(log.head().unwrap().sequence, 1);
}

#[test]
fn more_than_old_transaction_ceiling_streams_without_collecting_changes() {
    let file = File::new();
    let source = source();
    let mut log = EventLog::open_with_options(
        &file.0,
        source,
        Retention::default(),
        StorageOptions {
            chunk_bytes: 8192,
            ..StorageOptions::default()
        },
    )
    .unwrap();
    let initial = log.head().unwrap();
    log.begin_transaction(1, Lsn(20)).unwrap();
    let row = change(&"x".repeat(512));
    for _ in 0..10_001 {
        log.push_change(&row).unwrap();
    }
    log.finish_transaction(Lsn(20), Lsn(21)).unwrap();
    assert!(matches!(log.since(initial, 1), Err(Error::ReadBudget)));
    let mut count = 0;
    let head = log
        .visit_transactions(initial, 1, |stored| {
            assert!(stored.header.payload_bytes > 4 * 1024 * 1024);
            stored.for_each_change(|item| {
                assert_eq!(item, row);
                count += 1;
                Ok(())
            })
        })
        .unwrap();
    assert_eq!(count, 10_001);
    assert_eq!(head, log.head().unwrap());
}

#[test]
fn failure_after_some_chunk_writes_cannot_publish_or_reuse_partial_staging() {
    let file = File::new();
    let source = source();
    let mut log =
        EventLog::open_with_options(&file.0, source.clone(), Retention::default(), options())
            .unwrap();
    let injector = Connection::open(&file.0).unwrap();
    injector.execute_batch("CREATE TRIGGER fail_chunk BEFORE INSERT ON batch_chunks WHEN NEW.ordinal>=2 BEGIN SELECT RAISE(ABORT,'injected chunk failure'); END;").unwrap();
    log.begin_transaction(1, Lsn(20)).unwrap();
    assert!(matches!(
        log.push_change(&change(&"x".repeat(1000))),
        Err(Error::Sqlite(_))
    ));
    assert!(log.stats().unwrap().staged_payload_bytes > 0);
    assert!(log.finish_transaction(Lsn(20), Lsn(21)).is_err());
    assert_eq!(log.head().unwrap().sequence, 0);
    assert_eq!(log.checkpoint().unwrap(), source.start);
    assert_eq!(log.stats().unwrap().staged_payload_bytes, 0);
    injector.execute_batch("DROP TRIGGER fail_chunk").unwrap();
    log.begin_transaction(1, Lsn(20)).unwrap();
    log.push_change(&change("replay")).unwrap();
    log.finish_transaction(Lsn(20), Lsn(21)).unwrap();
    assert_eq!(log.head().unwrap().sequence, 1);
}
