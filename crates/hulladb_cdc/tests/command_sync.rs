//! Diagnostic coverage only: managed commands can still be inspected through WAL.
#[path = "support/commands.rs"]
mod commands;
use commands::CommandFixture;
use hulladb_cdc::{EventLog, Retention};
use hulladb_postgres::{Command, Uuid};
use std::time::Duration;

#[tokio::test]
#[ignore = "requires local PostgreSQL logical decoding"]
async fn diagnostic_capture_observes_commands_without_participating_in_delivery() {
    let mut f = CommandFixture::create(0).await.unwrap();
    let mut capture = f.capture().await.unwrap();
    let mut log = EventLog::open(&f.log_path, f.source.clone(), Retention::default()).unwrap();
    let request = Uuid::now_v7();
    let command = Command::Create {
        id: Uuid::now_v7(),
        title: "diagnostic".into(),
        completed: false,
    };
    let receipt = f
        .store
        .execute(f.session, request, command.clone())
        .await
        .unwrap();
    assert_eq!(
        f.store.execute(f.session, request, command).await.unwrap(),
        receipt
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while log.head().unwrap().sequence < 1 {
        tokio::time::timeout_at(deadline, capture.next(&mut log))
            .await
            .unwrap()
            .unwrap();
    }
    let cursor = hulladb_cdc::Cursor {
        source_id: f.source.source_id,
        sequence: 0,
    };
    let batches = log.since(cursor, 2).unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].batch.changes.len(), 1);
    capture.close().await.unwrap();
    f.cleanup().await.unwrap();
}
