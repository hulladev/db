mod support;

use hulladb_cdc::{Capture, Change, Cursor, Error, EventLog, Lsn, Progress, Retention, Value};
use support::Fixture;
use uuid::Uuid;

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

async fn idle(capture: &mut Capture, log: &mut EventLog) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(100);
    while let Ok(progress) = tokio::time::timeout_at(deadline, capture.next(log)).await {
        let progress = progress.unwrap();
        assert!(progress.appended.is_empty());
        assert_eq!(progress.replayed, 0);
    }
}

async fn transaction(
    capture: &mut Capture,
    log: &mut EventLog,
    feedback: bool,
) -> Result<Progress, Error> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let progress = tokio::time::timeout_at(deadline, async {
            if feedback {
                capture.next(log).await
            } else {
                capture.persist_next(log).await
            }
        })
        .await
        .expect("stream stalled")?;
        if !progress.appended.is_empty() || progress.replayed > 0 {
            return Ok(progress);
        }
    }
}

async fn acknowledged(f: &Fixture, checkpoint: Lsn) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let confirmed =
            f.db.query_one(
                "SELECT confirmed_flush_lsn::text FROM pg_replication_slots WHERE slot_name=$1",
                &[&f.source.slot],
            )
            .await
            .unwrap()
            .get::<_, String>(0)
            .parse::<Lsn>()
            .unwrap();
        if confirmed >= checkpoint {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "feedback stalled");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

#[tokio::test]
#[ignore = "requires local PostgreSQL with logical replication; make cdc-test"]
async fn external_writes_commit_boundaries_and_rollback() {
    let f = Fixture::create().await.unwrap();
    let mut log = EventLog::open(&f.path, f.source.clone(), Retention::default()).unwrap();
    let initial = log.head().unwrap();
    let mut capture = Capture::connect(&f.url, &f.source).await.unwrap();
    idle(&mut capture, &mut log).await;
    f.db.batch_execute(&format!(
        "BEGIN; INSERT INTO {}.tasks VALUES(1,'first',false),(2,'second',true); COMMIT;
        BEGIN; INSERT INTO {}.tasks VALUES(3,'rolled back',false); ROLLBACK;
        INSERT INTO {}.unpublished VALUES(99);",
        f.schema, f.schema, f.schema
    ))
    .await
    .unwrap();
    drain(&mut capture, &mut log, 1).await;
    let first = log.since(initial, 100).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].batch.changes.len(), 2);
    assert!(
        matches!(&first[0].batch.changes[0], Change::Insert { row, .. } if row[1] == Value::Text("first".into()))
    );
    assert_eq!(log.checkpoint().unwrap(), first[0].batch.end);
    idle(&mut capture, &mut log).await;
    f.db.batch_execute(&format!("BEGIN; UPDATE {}.tasks SET title='updated' WHERE id=1; DELETE FROM {}.tasks WHERE id=2; COMMIT;", f.schema, f.schema)).await.unwrap();
    drain(&mut capture, &mut log, 2).await;
    let second = log.since(first[0].cursor, 100).unwrap();
    assert!(
        matches!(&second[0].batch.changes[0], Change::Update { row, .. } if row[1] == Value::Text("updated".into()))
    );
    assert!(
        matches!(&second[0].batch.changes[1], Change::Delete { old, .. } if old[0] == Value::Text("2".into()))
    );
    f.db.batch_execute(&format!("TRUNCATE {}.tasks", f.schema))
        .await
        .unwrap();
    drain(&mut capture, &mut log, 3).await;
    assert!(matches!(
        log.since(second[0].cursor, 100).unwrap()[0].batch.changes[0],
        Change::Truncate { .. }
    ));
    capture.close().await.unwrap();
    drop(log);
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL with logical replication; make cdc-test"]
async fn failed_local_persistence_does_not_ack_and_retry_is_safe() {
    let f = Fixture::create().await.unwrap();
    let mut log = EventLog::open(&f.path, f.source.clone(), Retention::default()).unwrap();
    let mut capture = Capture::connect(&f.url, &f.source).await.unwrap();
    f.db.batch_execute(&format!(
        "INSERT INTO {}.tasks VALUES(1,'retry',false)",
        f.schema
    ))
    .await
    .unwrap();
    let injector = rusqlite::Connection::open(&f.path).unwrap();
    injector.execute_batch("CREATE TRIGGER fail_checkpoint BEFORE UPDATE ON metadata BEGIN SELECT RAISE(ABORT,'injected'); END").unwrap();
    assert!(transaction(&mut capture, &mut log, true).await.is_err());
    assert_eq!(log.checkpoint().unwrap(), f.source.start);
    assert_eq!(log.head().unwrap().sequence, 0);
    let confirmed =
        f.db.query_one(
            "SELECT confirmed_flush_lsn::text FROM pg_replication_slots WHERE slot_name=$1",
            &[&f.source.slot],
        )
        .await
        .unwrap()
        .get::<_, String>(0)
        .parse::<Lsn>()
        .unwrap();
    assert_eq!(confirmed, f.source.start);
    injector
        .execute_batch("DROP TRIGGER fail_checkpoint")
        .unwrap();
    drain(&mut capture, &mut log, 1).await;
    idle(&mut capture, &mut log).await;
    capture.close().await.unwrap();
    drop(log);
    drop(injector);
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL with logical replication; make cdc-test"]
async fn durable_before_ack_replay_reopen_and_exclusive_owner() {
    let f = Fixture::create().await.unwrap();
    let mut log = EventLog::open(&f.path, f.source.clone(), Retention::default()).unwrap();
    let mut capture = Capture::connect(&f.url, &f.source).await.unwrap();
    assert!(matches!(
        Capture::connect(&f.url, &f.source).await,
        Err(Error::OwnerBusy)
    ));
    f.db.batch_execute(&format!(
        "INSERT INTO {}.tasks VALUES(1,'replay',false)",
        f.schema
    ))
    .await
    .unwrap();
    let report = transaction(&mut capture, &mut log, false).await.unwrap();
    assert_eq!(report.appended.len(), 1);
    let confirmed =
        f.db.query_one(
            "SELECT confirmed_flush_lsn::text FROM pg_replication_slots WHERE slot_name=$1",
            &[&f.source.slot],
        )
        .await
        .unwrap()
        .get::<_, String>(0)
        .parse::<Lsn>()
        .unwrap();
    assert_eq!(confirmed, f.source.start);
    // Hard stop after the durable SQLite commit, before any feedback reaches PostgreSQL.
    capture.close().await.unwrap();
    drop(log);
    let mut log = EventLog::open(&f.path, f.source.clone(), Retention::default()).unwrap();
    let mut capture = Capture::connect(&f.url, &f.source).await.unwrap();
    let report = transaction(&mut capture, &mut log, true).await.unwrap();
    assert!(report.appended.is_empty());
    assert_eq!(report.replayed, 1);
    assert_eq!(log.stats().unwrap().batches, 1);
    let mut other = f.source.clone();
    other.generation = Uuid::now_v7();
    let other_fixture = Fixture::create().await.unwrap();
    let mut wrong_log = EventLog::open(&other_fixture.path, other, Retention::default()).unwrap();
    assert!(matches!(
        capture.next(&mut wrong_log).await,
        Err(Error::SourceMismatch)
    ));
    drop(wrong_log);
    other_fixture.cleanup().await.unwrap();
    capture.close().await.unwrap();
    drop(log);
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL with logical replication; make cdc-test"]
async fn slot_ahead_and_missing_slot_require_explicit_reset() {
    let f = Fixture::create().await.unwrap();
    let mut log = EventLog::open(&f.path, f.source.clone(), Retention::default()).unwrap();
    f.db.batch_execute(&format!(
        "INSERT INTO {}.tasks VALUES(1,'ahead',false)",
        f.schema
    ))
    .await
    .unwrap();
    f.db.query("SELECT * FROM pg_logical_slot_get_binary_changes($1,NULL,NULL,'proto_version','1','publication_names',$2)", &[&f.source.slot, &f.source.publication]).await.unwrap();
    let mut capture = Capture::connect(&f.url, &f.source).await.unwrap();
    assert!(matches!(
        capture.next(&mut log).await,
        Err(Error::SlotAhead) | Err(Error::SourceUnavailable)
    ));
    assert_eq!(log.head().unwrap().sequence, 0);
    capture.close().await.unwrap();
    f.db.query_one("SELECT pg_drop_replication_slot($1)", &[&f.source.slot])
        .await
        .unwrap();
    let mut capture = Capture::connect(&f.url, &f.source).await.unwrap();
    assert!(matches!(
        capture.next(&mut log).await,
        Err(Error::SourceUnavailable)
    ));
    capture.close().await.unwrap();
    // Recreate only for fixture cleanup; it is never silently recreated by Capture.
    f.db.query_one(
        "SELECT * FROM pg_create_logical_replication_slot($1,'pgoutput')",
        &[&f.source.slot],
    )
    .await
    .unwrap();
    drop(log);
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL with logical replication; make cdc-test"]
async fn retention_does_not_pin_ack_and_toast_remains_explicit() {
    let f = Fixture::create().await.unwrap();
    let mut log = EventLog::open(
        &f.path,
        f.source.clone(),
        Retention {
            max_batches: 2,
            ..Retention::default()
        },
    )
    .unwrap();
    let initial = log.head().unwrap();
    let mut capture = Capture::connect(&f.url, &f.source).await.unwrap();
    let large: String = (0..5000).map(|i| format!("{i:08x}")).collect();
    f.db.execute(
        &format!("INSERT INTO {}.tasks VALUES(1,$1,false)", f.schema),
        &[&large],
    )
    .await
    .unwrap();
    drain(&mut capture, &mut log, 1).await;
    f.db.batch_execute(&format!(
        "UPDATE {}.tasks SET completed=true WHERE id=1",
        f.schema
    ))
    .await
    .unwrap();
    drain(&mut capture, &mut log, 2).await;
    let events = log.since(initial, 100).unwrap();
    assert!(
        matches!(&events[1].batch.changes[0], Change::Update { row, .. } if matches!(&row[1], Value::UnchangedToast | Value::Text(_)))
    );
    f.db.batch_execute(&format!("DELETE FROM {}.tasks WHERE id=1", f.schema))
        .await
        .unwrap();
    drain(&mut capture, &mut log, 3).await;
    assert!(matches!(log.since(initial, 100), Err(Error::CursorExpired)));
    assert_eq!(log.stats().unwrap().batches, 2);
    acknowledged(&f, log.checkpoint().unwrap()).await;
    let confirmed =
        f.db.query_one(
            "SELECT confirmed_flush_lsn::text FROM pg_replication_slots WHERE slot_name=$1",
            &[&f.source.slot],
        )
        .await
        .unwrap()
        .get::<_, String>(0)
        .parse::<Lsn>()
        .unwrap();
    assert_eq!(confirmed, log.checkpoint().unwrap());
    assert!(matches!(
        log.since(
            Cursor {
                generation: Uuid::now_v7(),
                sequence: 0
            },
            1
        ),
        Err(Error::InvalidCursor)
    ));
    capture.close().await.unwrap();
    drop(log);
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL with logical replication; make cdc-test"]
async fn quiet_publication_progress_preserves_long_running_transaction() {
    let f = Fixture::create().await.unwrap();
    let mut log = EventLog::open(&f.path, f.source.clone(), Retention::default()).unwrap();
    let mut capture = Capture::connect(&f.url, &f.source).await.unwrap();
    f.db.batch_execute(&format!(
        "BEGIN; INSERT INTO {}.tasks VALUES(1,'open transaction',false)",
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
            "INSERT INTO {}.unpublished SELECT generate_series(1,100)",
            f.schema
        ))
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let report = loop {
        let report = tokio::time::timeout_at(deadline, capture.next(&mut log))
            .await
            .unwrap()
            .unwrap();
        assert!(report.appended.is_empty());
        if report.checkpoint > f.source.start {
            break report;
        }
    };
    assert_eq!(log.head().unwrap().sequence, 0);
    acknowledged(&f, report.checkpoint).await;
    let confirmed = writer
        .query_one(
            "SELECT confirmed_flush_lsn::text FROM pg_replication_slots WHERE slot_name=$1",
            &[&f.source.slot],
        )
        .await
        .unwrap()
        .get::<_, String>(0)
        .parse::<Lsn>()
        .unwrap();
    assert_eq!(confirmed, report.checkpoint);
    f.db.batch_execute("COMMIT").await.unwrap();
    drain(&mut capture, &mut log, 1).await;
    assert_eq!(log.stats().unwrap().batches, 1);
    connection.abort();
    capture.close().await.unwrap();
    drop(log);
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL with logical replication; make cdc-test"]
async fn triggers_and_concurrent_external_transactions_capture_actual_results() {
    let f = Fixture::create().await.unwrap();
    let mut log = EventLog::open(&f.path, f.source.clone(), Retention::default()).unwrap();
    let initial = log.head().unwrap();
    let mut capture = Capture::connect(&f.url, &f.source).await.unwrap();
    f.db.batch_execute(&format!("CREATE FUNCTION {}.canonicalize() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.title=upper(NEW.title); RETURN NEW; END; $$;
        CREATE TRIGGER canonicalize BEFORE INSERT ON {}.tasks FOR EACH ROW EXECUTE FUNCTION {}.canonicalize()", f.schema, f.schema, f.schema)).await.unwrap();
    let mut writers = tokio::task::JoinSet::new();
    for id in 1_i64..=4 {
        let url = f.url.clone();
        let sql = format!(
            "INSERT INTO {}.tasks VALUES($1,'canonical result',false)",
            f.schema
        );
        writers.spawn(async move {
            let (writer, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
                .await
                .unwrap();
            let connection = tokio::spawn(connection);
            writer.execute(&sql, &[&id]).await.unwrap();
            connection.abort();
        });
    }
    while let Some(result) = writers.join_next().await {
        result.unwrap();
    }
    drain(&mut capture, &mut log, 4).await;
    let events = log.since(initial, 100).unwrap();
    assert_eq!(events.len(), 4);
    for event in &events {
        assert!(
            matches!(&event.batch.changes[0], Change::Insert { row, .. } if row[1] == Value::Text("CANONICAL RESULT".into()))
        );
    }
    for pair in events.windows(2) {
        assert!(pair[0].batch.end <= pair[1].batch.commit);
    }
    capture.close().await.unwrap();
    drop(log);
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL with logical replication; make cdc-test"]
async fn oversized_transaction_halts_without_partial_history_or_ack() {
    let f = Fixture::create().await.unwrap();
    let mut log = EventLog::open(&f.path, f.source.clone(), Retention::default()).unwrap();
    let mut capture = Capture::connect(&f.url, &f.source).await.unwrap();
    f.db.batch_execute(&format!(
        "INSERT INTO {}.tasks SELECT i,repeat('x',2048),false FROM generate_series(1,2100) i",
        f.schema
    ))
    .await
    .unwrap();
    assert!(matches!(
        transaction(&mut capture, &mut log, true).await,
        Err(Error::Limit)
    ));
    assert_eq!(log.head().unwrap().sequence, 0);
    assert_eq!(log.checkpoint().unwrap(), f.source.start);
    assert_eq!(log.stats().unwrap().batches, 0);
    let confirmed =
        f.db.query_one(
            "SELECT confirmed_flush_lsn::text FROM pg_replication_slots WHERE slot_name=$1",
            &[&f.source.slot],
        )
        .await
        .unwrap()
        .get::<_, String>(0)
        .parse::<Lsn>()
        .unwrap();
    assert_eq!(confirmed, f.source.start);
    capture.close().await.unwrap();
    drop(log);
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL with logical replication; make cdc-test"]
async fn cancelled_wait_and_disconnected_stream_resume_without_skipping_buffered_commits() {
    let f = Fixture::create().await.unwrap();
    let mut log = EventLog::open(&f.path, f.source.clone(), Retention::default()).unwrap();
    let initial = log.head().unwrap();
    let mut capture = Capture::connect(&f.url, &f.source).await.unwrap();
    // Timeout cancels the wait, but retains the live socket and decoder.
    idle(&mut capture, &mut log).await;
    let pid: i32 =
        f.db.query_one(
            "SELECT active_pid FROM pg_replication_slots WHERE slot_name=$1",
            &[&f.source.slot],
        )
        .await
        .unwrap()
        .get(0);
    for id in 1_i64..=2 {
        f.db.execute(
            &format!("INSERT INTO {}.tasks VALUES($1,'queued',false)", f.schema),
            &[&id],
        )
        .await
        .unwrap();
    }
    transaction(&mut capture, &mut log, false).await.unwrap();
    f.db.query_one("SELECT pg_terminate_backend($1)", &[&pid])
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    // Buffered complete transactions may still be persisted before the socket
    // failure surfaces. None are acknowledged by persist_next.
    loop {
        if tokio::time::timeout_at(deadline, capture.persist_next(&mut log))
            .await
            .unwrap()
            .is_err()
        {
            break;
        }
    }
    f.db.batch_execute(&format!(
        "INSERT INTO {}.tasks VALUES(3,'after disconnect',false)",
        f.schema
    ))
    .await
    .unwrap();
    drain(&mut capture, &mut log, 3).await;
    let events = log.since(initial, 100).unwrap();
    assert_eq!(events.len(), 3);
    for (index, event) in events.iter().enumerate() {
        assert!(matches!(&event.batch.changes[0], Change::Insert { row, .. }
            if row[0] == Value::Text((index + 1).to_string())));
    }
    let new_pid: i32 =
        f.db.query_one(
            "SELECT active_pid FROM pg_replication_slots WHERE slot_name=$1",
            &[&f.source.slot],
        )
        .await
        .unwrap()
        .get(0);
    assert_ne!(pid, new_pid);
    capture.close().await.unwrap();
    drop(log);
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL with logical replication; make cdc-test"]
async fn backpressure_keeps_connection_alive_without_acknowledging_unconsumed_rows() {
    let f = Fixture::create().await.unwrap();
    let mut log = EventLog::open(&f.path, f.source.clone(), Retention::default()).unwrap();
    let url = if f.url.starts_with("postgresql://") || f.url.starts_with("postgres://") {
        let separator = if f.url.contains('?') { '&' } else { '?' };
        format!(
            "{}{separator}options=-c%20wal_sender_timeout%3D500ms",
            f.url
        )
    } else {
        format!("{} options='-c wal_sender_timeout=500ms'", f.url)
    };
    let mut capture = Capture::connect(&url, &f.source).await.unwrap();
    idle(&mut capture, &mut log).await;
    let checkpoint = log.checkpoint().unwrap();
    acknowledged(&f, checkpoint).await;
    let pid: i32 =
        f.db.query_one(
            "SELECT active_pid FROM pg_replication_slots WHERE slot_name=$1",
            &[&f.source.slot],
        )
        .await
        .unwrap()
        .get(0);
    // Exceed the 32-event transport queue, then pause beyond wal_sender_timeout.
    f.db.batch_execute(&format!(
        "INSERT INTO {}.tasks SELECT i,repeat('x',2048),false FROM generate_series(1,128) i",
        f.schema
    ))
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let slot = f.db.query_one(
        "SELECT active_pid,confirmed_flush_lsn::text FROM pg_replication_slots WHERE slot_name=$1", &[&f.source.slot],
    ).await.unwrap();
    assert_eq!(slot.get::<_, i32>(0), pid);
    assert_eq!(slot.get::<_, String>(1).parse::<Lsn>().unwrap(), checkpoint);
    assert_eq!(log.head().unwrap().sequence, 0);
    drain(&mut capture, &mut log, 1).await;
    assert_eq!(
        log.since(
            hulladb_cdc::Cursor {
                generation: f.source.generation,
                sequence: 0
            },
            1
        )
        .unwrap()[0]
            .batch
            .changes
            .len(),
        128
    );
    capture.close().await.unwrap();
    drop(log);
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL with logical replication; make cdc-test"]
async fn logical_messages_do_not_become_row_delivery_batches() {
    let f = Fixture::create().await.unwrap();
    let mut log = EventLog::open(&f.path, f.source.clone(), Retention::default()).unwrap();
    let mut capture = Capture::connect(&f.url, &f.source).await.unwrap();
    f.db.batch_execute("SELECT pg_logical_emit_message(false,'other-service','ignored'); SELECT pg_logical_emit_message(true,'other-service','ignored')").await.unwrap();
    f.db.batch_execute(&format!(
        "INSERT INTO {}.tasks VALUES(1,'row after messages',false)",
        f.schema
    ))
    .await
    .unwrap();
    drain(&mut capture, &mut log, 1).await;
    assert_eq!(log.stats().unwrap().batches, 1);
    capture.close().await.unwrap();
    drop(log);
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL with logical replication; make cdc-test"]
async fn active_stream_rejects_replacing_its_log_with_an_older_copy() {
    let f = Fixture::create().await.unwrap();
    let mut log = EventLog::open(&f.path, f.source.clone(), Retention::default()).unwrap();
    let stale_path = f.path.with_extension("stale.sqlite");
    let mut stale = EventLog::open(&stale_path, f.source.clone(), Retention::default()).unwrap();
    let mut capture = Capture::connect(&f.url, &f.source).await.unwrap();
    f.db.batch_execute(&format!(
        "INSERT INTO {}.tasks VALUES(1,'persisted',false)",
        f.schema
    ))
    .await
    .unwrap();
    transaction(&mut capture, &mut log, false).await.unwrap();
    assert!(matches!(
        capture.next(&mut stale).await,
        Err(Error::SlotAhead)
    ));
    f.db.batch_execute(&format!(
        "INSERT INTO {}.tasks VALUES(2,'valid continuation',false)",
        f.schema
    ))
    .await
    .unwrap();
    drain(&mut capture, &mut log, 2).await;
    assert_eq!(log.stats().unwrap().batches, 2);
    assert_eq!(stale.head().unwrap().sequence, 0);
    capture.close().await.unwrap();
    drop(log);
    drop(stale);
    for suffix in ["", "-wal", "-shm"] {
        std::fs::remove_file(format!("{}{suffix}", stale_path.display())).ok();
    }
    f.cleanup().await.unwrap();
}
