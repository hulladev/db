#[path = "support/commands.rs"]
mod commands;
use commands::CommandFixture;
use hulladb_client::{ReplicaOptions, TaskReplica};
use hulladb_postgres::{Command, Error, Store, TaskQuery, Uuid};
use hulladb_sync::TaskFeed;
use std::time::Duration;

fn options() -> ReplicaOptions {
    ReplicaOptions {
        max_chunk_rows: 8,
        max_chunk_bytes: 32 * 1024,
        retained_deliveries_per_query: 2,
        busy_timeout: Duration::from_secs(1),
    }
}

#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn missed_wakeups_and_server_restart_recover_authorized_client_state_and_receipts() {
    let mut f = CommandFixture::create(0).await.unwrap();
    let id = Uuid::now_v7();
    let request = Uuid::now_v7();
    let command = Command::Create {
        id,
        title: "initial".into(),
        completed: false,
    };
    let receipt = f
        .store
        .execute(f.session, request, command.clone())
        .await
        .unwrap();
    let other = f.store.create_workspace(f.session.principal).await.unwrap();
    f.store
        .execute(
            other,
            Uuid::now_v7(),
            Command::Create {
                id,
                title: "private".into(),
                completed: false,
            },
        )
        .await
        .unwrap();
    let query = TaskQuery {
        completed: None,
        after: None,
        limit: 100,
    };
    let mut feed = TaskFeed::new(&f.store, 1).await.unwrap();
    let mut subscription = feed
        .subscribe(&mut f.store, f.session, query)
        .await
        .unwrap();
    let initial = subscription.next(&f.store).await.unwrap();
    let mut client = TaskReplica::open(&f.client_path, f.session, options()).unwrap();
    client.register(query, initial.cursor.subscription).unwrap();
    client.apply(&initial).unwrap();
    assert_eq!(
        f.store.execute(f.session, request, command).await.unwrap(),
        receipt
    );
    let before = f.store.managed_head(f.session).await.unwrap();
    f.store
        .execute(
            f.session,
            Uuid::now_v7(),
            Command::Patch {
                id,
                expected_version: 1,
                title: Some("committed without wakeup".into()),
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
    assert_eq!(
        f.store
            .managed_head(f.session)
            .await
            .unwrap()
            .revision
            .sequence,
        before.revision.sequence + 1
    );
    // No notification or per-command broadcast happened. A later poll repairs state.
    feed.refresh(&mut f.store).await.unwrap();
    client
        .apply(&subscription.next(&f.store).await.unwrap())
        .unwrap();
    assert_eq!(
        client.read(f.session, query).unwrap().tasks[0].title,
        "committed without wakeup"
    );
    drop(subscription);
    drop(feed);
    drop(client);
    f.store = Store::connect(&f.url).await.unwrap();
    assert_eq!(
        f.store.receipt(f.session, request).await.unwrap(),
        Some(receipt)
    );
    // Commit while no sync service exists. Restart reconstructs state from PostgreSQL.
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
    let mut feed = TaskFeed::new(&f.store, 1).await.unwrap();
    let mut subscription = feed
        .subscribe(&mut f.store, f.session, query)
        .await
        .unwrap();
    let fresh = subscription.next(&f.store).await.unwrap();
    assert_ne!(fresh.cursor.subscription, initial.cursor.subscription);
    let mut client = TaskReplica::open(&f.client_path, f.session, options()).unwrap();
    client.register(query, fresh.cursor.subscription).unwrap();
    client.apply(&fresh).unwrap();
    assert!(client.read(f.session, query).unwrap().tasks.is_empty());
    drop(client);
    f.store
        .delete_fixture_workspace(other.workspace)
        .await
        .unwrap();
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn receipts_become_visible_even_when_filtered_top_n_results_do_not_change() {
    let mut f = CommandFixture::create(0).await.unwrap();
    f.store
        .execute(
            f.session,
            Uuid::now_v7(),
            Command::Create {
                id: Uuid::from_u128(1),
                title: "first".into(),
                completed: false,
            },
        )
        .await
        .unwrap();
    let query = TaskQuery {
        completed: Some(false),
        after: None,
        limit: 1,
    };
    let mut feed = TaskFeed::new(&f.store, 1).await.unwrap();
    let mut sub = feed
        .subscribe(&mut f.store, f.session, query)
        .await
        .unwrap();
    let initial = sub.next(&f.store).await.unwrap();
    let mut client = TaskReplica::open(&f.client_path, f.session, options()).unwrap();
    client.register(query, initial.cursor.subscription).unwrap();
    client.apply(&initial).unwrap();
    let mut receipts = Vec::new();
    for (id, completed) in [(2, false), (3, true)] {
        let receipt = f
            .store
            .execute(
                f.session,
                Uuid::now_v7(),
                Command::Create {
                    id: Uuid::from_u128(id),
                    title: "outside this page".into(),
                    completed,
                },
            )
            .await
            .unwrap();
        assert!(
            !client
                .is_visible(f.session, query, receipt.visibility_fence())
                .unwrap()
        );
        feed.refresh(&mut f.store).await.unwrap();
        receipts.push(receipt);
    }
    // This slow receiver skips the first revision's delivery; the latest complete
    // result proves both commits were evaluated even though its rows are unchanged.
    let latest = tokio::time::timeout(Duration::from_secs(5), sub.next(&f.store))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.result, initial.result);
    assert!(latest.cursor.sequence > initial.cursor.sequence + 1);
    assert_eq!(latest.visibility, receipts[1].visibility_fence());
    for receipt in &receipts {
        assert!(
            !client
                .is_visible(f.session, query, receipt.visibility_fence())
                .unwrap()
        );
    }
    client.apply(&latest).unwrap();
    drop(client);
    let client = TaskReplica::open(&f.client_path, f.session, options()).unwrap();
    for receipt in receipts {
        assert!(
            client
                .is_visible(f.session, query, receipt.visibility_fence())
                .unwrap()
        );
    }
    assert_eq!(
        client.read(f.session, query).unwrap().tasks[0].title,
        "first"
    );
    drop(client);
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run serially because it rotates source metadata"]
async fn recovered_receipt_keeps_its_commit_source_after_reset() {
    let mut f = CommandFixture::create(0).await.unwrap();
    let request = Uuid::now_v7();
    let command = Command::Create {
        id: Uuid::now_v7(),
        title: "committed".into(),
        completed: false,
    };
    let receipt = f
        .store
        .execute(f.session, request, command.clone())
        .await
        .unwrap();
    let source = receipt.source;
    let replacement = Uuid::now_v7();
    f.db.execute(
        "UPDATE _hulladb_task_probe.metadata SET source=$1 WHERE singleton",
        &[&replacement],
    )
    .await
    .unwrap();
    let recovered = f.store.receipt(f.session, request).await;
    let duplicate = f.store.execute(f.session, request, command).await;
    let query = TaskQuery {
        completed: None,
        after: None,
        limit: 100,
    };
    let mut feed = TaskFeed::new(&f.store, 1).await.unwrap();
    let mut sub = feed
        .subscribe(&mut f.store, f.session, query)
        .await
        .unwrap();
    let fresh = sub.next(&f.store).await.unwrap();
    // Restore metadata before assertions so this fixture cannot contaminate later cases.
    f.db.execute(
        "UPDATE _hulladb_task_probe.metadata SET source=$1 WHERE singleton",
        &[&source],
    )
    .await
    .unwrap();
    assert_eq!(recovered.unwrap(), Some(receipt.clone()));
    assert_eq!(duplicate.unwrap(), receipt);
    assert_eq!(fresh.visibility.source, replacement);
    let mut client = TaskReplica::open(&f.client_path, f.session, options()).unwrap();
    client.register(query, fresh.cursor.subscription).unwrap();
    client.apply(&fresh).unwrap();
    assert!(matches!(
        client.is_visible(f.session, query, receipt.visibility_fence()),
        Err(hulladb_client::Error::InvalidVisibility)
    ));
    drop(client);
    f.cleanup().await.unwrap();
}
