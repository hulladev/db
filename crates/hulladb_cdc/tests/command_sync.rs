#[path = "support/commands.rs"]
mod commands;

use commands::CommandFixture;
use hulladb_cdc::{Capture, EventLog, Replica, Retention, Value};
use hulladb_postgres::{Command, Error, Uuid};

async fn publish(capture: &mut Capture, log: &mut EventLog, replica: &mut Replica, expected: i64) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while log.head().unwrap().sequence < expected {
        tokio::time::timeout_at(deadline, capture.next(log))
            .await
            .unwrap()
            .unwrap();
        assert!(tokio::time::Instant::now() < deadline, "capture stalled");
        tokio::task::yield_now().await;
    }
    replica.catch_up(log, 100).unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; make cdc-test"]
async fn real_commands_external_writes_scoped_keys_and_restart_share_one_cdc_path() {
    let mut f = CommandFixture::create(0).await.unwrap();
    let id = Uuid::now_v7();
    let create = Command::Create {
        id,
        title: "before snapshot".into(),
        completed: false,
    };
    let request = Uuid::now_v7();
    let receipt = f
        .store
        .execute(f.session, request, create.clone())
        .await
        .unwrap();
    let other = f.store.create_workspace(f.session.principal).await.unwrap();
    // The same logical ID in another scope must remain a distinct physical row.
    f.store
        .execute(other, Uuid::now_v7(), create.clone())
        .await
        .unwrap();
    let snapshot = f.bootstrap().await.unwrap().read().await.unwrap();
    let mut replica = Replica::initialize(&f.replica_path, snapshot).unwrap();
    let mut log = EventLog::open(&f.log_path, f.source.clone(), Retention::default()).unwrap();
    let mut capture = Capture::connect(&f.url, &f.source).await.unwrap();
    assert_eq!(replica.view(f.table, 1000).unwrap().rows.len(), 2);
    assert_eq!(
        f.store.execute(f.session, request, create).await.unwrap(),
        receipt
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(100);
    while let Ok(progress) = tokio::time::timeout_at(deadline, capture.next(&mut log)).await {
        assert!(progress.unwrap().appended.is_empty());
    }
    assert_eq!(
        log.head().unwrap().sequence,
        0,
        "copied rows and receipt replay must not emit mutations"
    );
    f.store
        .execute(
            f.session,
            Uuid::now_v7(),
            Command::Patch {
                id,
                expected_version: 1,
                title: Some("command update".into()),
                completed: Some(true),
            },
        )
        .await
        .unwrap();
    let failed = Uuid::now_v7();
    assert!(matches!(
        f.store
            .execute(
                f.session,
                failed,
                Command::Delete {
                    id,
                    expected_version: 1
                }
            )
            .await,
        Err(Error::Conflict)
    ));
    assert!(f.store.receipt(f.session, failed).await.unwrap().is_none());
    // External SQL is captured by the same WAL path, without a command log.
    f.db.execute("UPDATE _hulladb_task_probe.tasks SET title='external update',version=version+1 WHERE workspace=$1 AND id=$2", &[&other.workspace, &id]).await.unwrap();
    publish(&mut capture, &mut log, &mut replica, 2).await;
    let view = replica.view(f.table, 1000).unwrap();
    assert!(view.complete);
    let own: Vec<_> = view
        .rows
        .iter()
        .filter(|row| row[0] == Value::Text(f.session.workspace.to_string()))
        .collect();
    let theirs: Vec<_> = view
        .rows
        .iter()
        .filter(|row| row[0] == Value::Text(other.workspace.to_string()))
        .collect();
    assert_eq!(own.len(), 1);
    assert_eq!(theirs.len(), 1);
    assert_eq!(own[0][2], Value::Text("command update".into()));
    assert_eq!(own[0][4], Value::Text("2".into()));
    assert_eq!(theirs[0][2], Value::Text("external update".into()));
    f.store
        .execute(
            f.session,
            Uuid::now_v7(),
            Command::Delete {
                id,
                expected_version: 2,
            },
        )
        .await
        .unwrap();
    publish(&mut capture, &mut log, &mut replica, 3).await;
    capture.close().await.unwrap();
    drop(replica);
    drop(log);
    let log = EventLog::open(&f.log_path, f.source.clone(), Retention::default()).unwrap();
    let mut replica = Replica::open(&f.replica_path, &f.source).unwrap();
    assert_eq!(replica.catch_up(&log, 100).unwrap(), 0);
    let view = replica.view(f.table, 1000).unwrap();
    assert_eq!(view.cursor, log.head().unwrap());
    assert!(
        !view
            .rows
            .iter()
            .any(|row| row[0] == Value::Text(f.session.workspace.to_string()))
    );
    assert!(
        view.rows
            .iter()
            .any(|row| row[0] == Value::Text(other.workspace.to_string()))
    );
    drop(replica);
    drop(log);
    f.store
        .delete_fixture_workspace(other.workspace)
        .await
        .unwrap();
    f.cleanup().await.unwrap();
}
