mod support;

use hulladb_cdc::{Capture, Error, EventLog, Replica, Retention, SnapshotReader, Value};
use support::Fixture;

async fn drain(capture: &mut Capture, log: &mut EventLog, expected: i64) {
    let until = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while log.head().unwrap().sequence < expected {
        tokio::time::timeout_at(until, capture.next(log))
            .await
            .unwrap()
            .unwrap();
        assert!(
            tokio::time::Instant::now() < until,
            "CDC did not reach expected head"
        );
        tokio::task::yield_now().await;
    }
}

async fn oid(f: &Fixture, table: &str) -> u32 {
    f.db.query_one("SELECT c.oid FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1 AND c.relname=$2", &[&f.schema,&table]).await.unwrap().get(0)
}

fn path(f: &Fixture) -> std::path::PathBuf {
    f.path.with_extension("replica.sqlite")
}
fn cleanup(path: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; make cdc-test"]
async fn overlapping_snapshot_external_writes_open_transaction_and_restart() {
    let f = Fixture::create_with_seed(true).await.unwrap();
    // This commit is in both the snapshot and the retained stream. Applying it
    // twice would produce duplicate-key failure rather than hide the overlap.
    f.db.batch_execute(&format!(
        "INSERT INTO {}.tasks VALUES(1,'copied',false)",
        f.schema
    ))
    .await
    .unwrap();
    let (writer, connection) = tokio_postgres::connect(&f.url, tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    writer
        .batch_execute(&format!(
            "BEGIN; INSERT INTO {}.tasks VALUES(2,'open',false)",
            f.schema
        ))
        .await
        .unwrap();
    let reader = SnapshotReader::begin(&f.url, &f.source).await.unwrap();
    assert!(matches!(
        Capture::connect(&f.url, &f.source).await,
        Err(Error::OwnerBusy)
    ));
    writer.batch_execute("COMMIT").await.unwrap();
    writer.batch_execute(&format!("BEGIN; UPDATE {}.tasks SET title='after snapshot' WHERE id=1; INSERT INTO {}.tasks VALUES(3,'new',true); COMMIT",f.schema,f.schema)).await.unwrap();
    let snapshot = reader.read().await.unwrap();
    let table = oid(&f, "tasks").await;
    let replica_path = path(&f);
    let mut replica = Replica::initialize(&replica_path, snapshot).unwrap();
    let view = replica.view(table, 100).unwrap();
    assert_eq!(view.cursor.sequence, 0);
    assert!(view.complete);
    assert_eq!(view.rows.len(), 2);
    assert_eq!(view.rows[0][1], Value::Text("before slot".into()));
    assert_eq!(view.rows[1][1], Value::Text("copied".into()));
    let mut log = EventLog::open(&f.path, f.source.clone(), Retention::default()).unwrap();
    let mut capture = Capture::connect(&f.url, &f.source).await.unwrap();
    // Persist/ack independently of serving, then restart both durable consumers.
    drain(&mut capture, &mut log, 3).await;
    capture.close().await.unwrap();
    drop(log);
    drop(replica);
    let log = EventLog::open(&f.path, f.source.clone(), Retention::default()).unwrap();
    replica = Replica::open(&replica_path, &f.source).unwrap();
    assert_eq!(replica.catch_up(&log, 100).unwrap(), 3);
    let view = replica.view(table, 100).unwrap();
    assert_eq!(view.rows.len(), 4);
    assert_eq!(view.cursor, log.head().unwrap());
    assert_eq!(view.rows[0][1], Value::Text("before slot".into()));
    assert_eq!(view.rows[1][1], Value::Text("after snapshot".into()));
    assert_eq!(replica.catch_up(&log, 100).unwrap(), 0);
    assert!(!replica.view(table, 1).unwrap().complete);
    drop(replica);
    let replica = Replica::open(&replica_path, &f.source).unwrap();
    assert_eq!(replica.view(table, 100).unwrap().rows, view.rows);
    let mut other = f.source.clone();
    other.generation = uuid::Uuid::now_v7();
    assert!(matches!(
        Replica::open(&replica_path, &other),
        Err(Error::SourceMismatch)
    ));
    drop(replica);
    drop(log);
    connection.abort();
    cleanup(&replica_path);
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; make cdc-test"]
async fn multi_table_transactions_toast_key_changes_delete_and_truncate() {
    let f = Fixture::create().await.unwrap();
    f.db.batch_execute(&format!("CREATE TABLE {}.notes(id bigint PRIMARY KEY,body text); ALTER PUBLICATION {} ADD TABLE {}.notes",f.schema,f.source.publication,f.schema)).await.unwrap();
    let large: String = (0..5000).map(|i| format!("{i:08x}")).collect();
    f.db.execute(
        &format!("INSERT INTO {}.tasks VALUES(1,$1,false)", f.schema),
        &[&large],
    )
    .await
    .unwrap();
    let snapshot = SnapshotReader::begin(&f.url, &f.source)
        .await
        .unwrap()
        .read()
        .await
        .unwrap();
    let tasks = oid(&f, "tasks").await;
    let notes = oid(&f, "notes").await;
    let replica_path = path(&f);
    let mut replica = Replica::initialize(&replica_path, snapshot).unwrap();
    let mut log = EventLog::open(&f.path, f.source.clone(), Retention::default()).unwrap();
    let mut capture = Capture::connect(&f.url, &f.source).await.unwrap();
    drain(&mut capture, &mut log, 1).await;
    replica.catch_up(&log, 100).unwrap();
    f.db.batch_execute(&format!("BEGIN; UPDATE {}.tasks SET completed=true WHERE id=1; INSERT INTO {}.notes VALUES(1,NULL); COMMIT",f.schema,f.schema)).await.unwrap();
    drain(&mut capture, &mut log, 2).await;
    // Inject failure on the second relation: the first row and cursor must roll back.
    let injector = rusqlite::Connection::open(&replica_path).unwrap();
    injector.execute_batch(&format!("CREATE TRIGGER fail_rows BEFORE INSERT ON replica_rows WHEN NEW.relation_oid={notes} BEGIN SELECT RAISE(ABORT,'injected'); END")).unwrap();
    assert!(replica.catch_up(&log, 100).is_err());
    assert_eq!(replica.cursor().unwrap().sequence, 1);
    assert_eq!(
        replica.view(tasks, 100).unwrap().rows[0][2],
        Value::Text("f".into())
    );
    assert!(replica.view(notes, 100).unwrap().rows.is_empty());
    injector.execute_batch("DROP TRIGGER fail_rows").unwrap();
    replica.catch_up(&log, 100).unwrap();
    let row = &replica.view(tasks, 100).unwrap().rows[0];
    assert_eq!(row[1], Value::Text(large));
    assert_eq!(row[2], Value::Text("t".into()));
    assert_eq!(replica.view(notes, 100).unwrap().rows[0][1], Value::Null);
    f.db.batch_execute(&format!(
        "BEGIN; UPDATE {}.tasks SET id=2 WHERE id=1; DELETE FROM {}.notes WHERE id=1; COMMIT",
        f.schema, f.schema
    ))
    .await
    .unwrap();
    drain(&mut capture, &mut log, 3).await;
    replica.catch_up(&log, 100).unwrap();
    assert_eq!(
        replica.view(tasks, 100).unwrap().rows[0][0],
        Value::Text("2".into())
    );
    assert!(replica.view(notes, 100).unwrap().rows.is_empty());
    f.db.batch_execute(&format!("TRUNCATE {}.tasks,{}.notes", f.schema, f.schema))
        .await
        .unwrap();
    drain(&mut capture, &mut log, 4).await;
    replica.catch_up(&log, 100).unwrap();
    assert!(replica.view(tasks, 100).unwrap().rows.is_empty());
    assert!(replica.view(notes, 100).unwrap().rows.is_empty());
    capture.close().await.unwrap();
    drop(replica);
    drop(injector);
    drop(log);
    cleanup(&replica_path);
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; make cdc-test"]
async fn expired_handoff_schema_changes_and_acknowledged_slot_fail_explicitly() {
    let f = Fixture::create().await.unwrap();
    let snapshot = SnapshotReader::begin(&f.url, &f.source)
        .await
        .unwrap()
        .read()
        .await
        .unwrap();
    let replica_path = path(&f);
    let mut replica = Replica::initialize(&replica_path, snapshot).unwrap();
    let mut log = EventLog::open(
        &f.path,
        f.source.clone(),
        Retention {
            max_batches: 1,
            ..Retention::default()
        },
    )
    .unwrap();
    let mut capture = Capture::connect(&f.url, &f.source).await.unwrap();
    for id in 1_i64..=2 {
        f.db.execute(
            &format!("INSERT INTO {}.tasks VALUES($1,'late',false)", f.schema),
            &[&id],
        )
        .await
        .unwrap();
        drain(&mut capture, &mut log, id).await;
    }
    assert!(matches!(
        replica.catch_up(&log, 100),
        Err(Error::CursorExpired)
    ));
    assert_eq!(replica.cursor().unwrap().sequence, 0);
    capture.close().await.unwrap();
    assert!(matches!(
        SnapshotReader::begin(&f.url, &f.source).await,
        Err(Error::SlotAhead) | Err(Error::SourceUnavailable)
    ));
    drop(replica);
    drop(log);
    cleanup(&replica_path);
    f.cleanup().await.unwrap();

    let f = Fixture::create().await.unwrap();
    let snapshot = SnapshotReader::begin(&f.url, &f.source)
        .await
        .unwrap()
        .read()
        .await
        .unwrap();
    let replica_path = path(&f);
    let mut replica = Replica::initialize(&replica_path, snapshot).unwrap();
    let mut log = EventLog::open(&f.path, f.source.clone(), Retention::default()).unwrap();
    let mut capture = Capture::connect(&f.url, &f.source).await.unwrap();
    f.db.batch_execute(&format!("ALTER TABLE {}.tasks ADD COLUMN new_field text; INSERT INTO {}.tasks VALUES(1,'changed',false,'new')",f.schema,f.schema)).await.unwrap();
    drain(&mut capture, &mut log, 1).await;
    assert!(matches!(
        replica.catch_up(&log, 100),
        Err(Error::SourceMismatch)
    ));
    assert_eq!(replica.cursor().unwrap().sequence, 0);
    capture.close().await.unwrap();
    drop(replica);
    drop(log);
    cleanup(&replica_path);
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; make cdc-test"]
async fn filtered_publication_and_large_snapshot_are_never_marked_ready() {
    let f = Fixture::create().await.unwrap();
    f.db.batch_execute(&format!("ALTER PUBLICATION {} DROP TABLE {}.tasks; ALTER PUBLICATION {} ADD TABLE {}.tasks WHERE (completed)",f.source.publication,f.schema,f.source.publication,f.schema)).await.unwrap();
    let reader = SnapshotReader::begin(&f.url, &f.source).await.unwrap();
    assert!(matches!(reader.read().await, Err(Error::Invalid(_))));
    f.cleanup().await.unwrap();

    let f = Fixture::create().await.unwrap();
    f.db.batch_execute(&format!(
        "INSERT INTO {}.tasks SELECT i,repeat('x',2048),false FROM generate_series(1,8300) i",
        f.schema
    ))
    .await
    .unwrap();
    let reader = SnapshotReader::begin(&f.url, &f.source).await.unwrap();
    assert!(matches!(reader.read().await, Err(Error::Limit)));
    // Snapshot failure neither consumes the slot nor produces a usable replica.
    let confirmed: String =
        f.db.query_one(
            "SELECT confirmed_flush_lsn::text FROM pg_replication_slots WHERE slot_name=$1",
            &[&f.source.slot],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        confirmed.parse::<hulladb_cdc::Lsn>().unwrap(),
        f.source.start
    );
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; make cdc-test"]
async fn snapshot_locks_block_truncate_until_the_copy_is_complete() {
    let f = Fixture::create_with_seed(true).await.unwrap();
    let reader = SnapshotReader::begin(&f.url, &f.source).await.unwrap();
    let error =
        f.db.batch_execute(&format!(
            "BEGIN; LOCK TABLE {}.tasks IN ACCESS EXCLUSIVE MODE NOWAIT",
            f.schema
        ))
        .await
        .unwrap_err();
    assert_eq!(
        error.code(),
        Some(&tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE)
    );
    f.db.batch_execute("ROLLBACK").await.unwrap();
    let snapshot = reader.read().await.unwrap();
    let table = oid(&f, "tasks").await;
    let replica_path = path(&f);
    let mut replica = Replica::initialize(&replica_path, snapshot).unwrap();
    assert_eq!(replica.view(table, 100).unwrap().rows.len(), 1);
    let mut log = EventLog::open(&f.path, f.source.clone(), Retention::default()).unwrap();
    let mut capture = Capture::connect(&f.url, &f.source).await.unwrap();
    f.db.batch_execute(&format!("TRUNCATE {}.tasks", f.schema))
        .await
        .unwrap();
    drain(&mut capture, &mut log, 1).await;
    replica.catch_up(&log, 100).unwrap();
    assert!(replica.view(table, 100).unwrap().rows.is_empty());
    capture.close().await.unwrap();
    drop(replica);
    drop(log);
    cleanup(&replica_path);
    f.cleanup().await.unwrap();
}
