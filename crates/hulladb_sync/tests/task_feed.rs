#[path = "support/commands.rs"]
mod commands;
use commands::CommandFixture;
use hulladb_client::{ReplicaOptions, TaskReplica};
use hulladb_postgres::{Command, Error, Session, Store, TaskQuery, Uuid};
use hulladb_sync::{TaskFeed, TaskFeedError, TaskMessage, TaskResult, TaskSubscription};
use std::{sync::Arc, time::Duration};

fn query(limit: u16) -> TaskQuery {
    TaskQuery {
        completed: None,
        after: None,
        limit,
    }
}
fn create(id: u128, title: &str) -> Command {
    Command::Create {
        id: Uuid::from_u128(id),
        title: title.into(),
        completed: false,
    }
}
async fn receive(sub: &mut TaskSubscription, store: &Store) -> Arc<TaskMessage> {
    tokio::time::timeout(Duration::from_secs(5), sub.next(store))
        .await
        .unwrap()
        .unwrap()
}
fn tasks(message: &TaskMessage) -> &[hulladb_postgres::TaskSummary] {
    match &message.result {
        TaskResult::Snapshot { tasks } => tasks,
        other => panic!("expected snapshot, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn commits_during_client_bootstrap_are_detected_without_wakeups() {
    let mut f = CommandFixture::create(0).await.unwrap();
    f.store
        .execute(f.session, Uuid::now_v7(), create(1, "initial"))
        .await
        .unwrap();
    let q = query(100);
    let mut feed = TaskFeed::new(&f.store, 2).await.unwrap();
    let mut sub = feed.subscribe(&mut f.store, f.session, q).await.unwrap();
    let initial = receive(&mut sub, &f.store).await;
    let mut client = TaskReplica::open(
        &f.client_path,
        f.session,
        ReplicaOptions {
            max_chunk_rows: 8,
            max_chunk_bytes: 32 * 1024,
            retained_deliveries_per_query: 2,
            busy_timeout: Duration::from_secs(1),
        },
    )
    .unwrap();
    client.register(q, initial.cursor.subscription).unwrap();
    let mut writer = client
        .begin_snapshot(q, initial.cursor, initial.visibility)
        .unwrap();
    writer.append(tasks(&initial)).unwrap();
    f.store
        .execute(
            f.session,
            Uuid::now_v7(),
            Command::Patch {
                id: Uuid::from_u128(1),
                expected_version: 1,
                title: Some("during bootstrap".into()),
                completed: None,
            },
        )
        .await
        .unwrap();
    writer.finish().unwrap();
    assert_eq!(client.read(f.session, q).unwrap().tasks[0].title, "initial");
    feed.refresh(&mut f.store).await.unwrap();
    client
        .apply(receive(&mut sub, &f.store).await.as_ref())
        .unwrap();
    assert_eq!(
        client.read(f.session, q).unwrap().tasks[0].title,
        "during bootstrap"
    );
    // A new subscriber gets the current committed state and shares the same group.
    let mut ahead = feed.subscribe(&mut f.store, f.session, q).await.unwrap();
    assert_eq!(
        tasks(receive(&mut ahead, &f.store).await.as_ref())[0].title,
        "during bootstrap"
    );
    drop(client);
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn rows_and_revision_share_a_snapshot_even_when_a_commit_interrupts_the_query() {
    let mut f = CommandFixture::create(0).await.unwrap();
    f.store
        .execute(f.session, Uuid::now_v7(), create(1, "before"))
        .await
        .unwrap();
    let before = f.store.managed_head(f.session).await.unwrap();
    let tx = f.db.transaction().await.unwrap();
    tx.batch_execute("LOCK TABLE _hulladb_task_probe.tasks IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();
    let session = f.session;
    let mut reader = Store::connect(&f.url).await.unwrap();
    let pending =
        tokio::spawn(async move { reader.query_snapshot(session, query(1)).await.unwrap() });
    // Wait for the actual tasks read to block, after the read transaction established
    // its snapshot through policy/revision reads. No timing assumption about scheduling.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let blocked: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE relation='_hulladb_task_probe.tasks'::regclass AND mode='AccessShareLock' AND NOT granted)", &[]).await.unwrap().get(0);
        if blocked {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "snapshot did not reach blocked read"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Administrative test injection models the same atomic data/revision commit
    // while the old read snapshot is in flight. Production mutations use Store.
    tx.execute(
        "UPDATE _hulladb_task_probe.tasks SET title='after',version=version+1 WHERE workspace=$1",
        &[&session.workspace],
    )
    .await
    .unwrap();
    tx.execute(
        "UPDATE _hulladb_task_probe.workspaces SET sequence=sequence+1 WHERE id=$1",
        &[&session.workspace],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let old = pending.await.unwrap();
    assert_eq!(old.revision, before.revision);
    assert_eq!(old.tasks[0].title, "before");
    let fresh = f.store.query_snapshot(session, query(1)).await.unwrap();
    assert_eq!(fresh.revision.sequence, before.revision.sequence + 1);
    assert_eq!(fresh.tasks[0].title, "after");
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn top_n_refill_coalescing_projection_and_workspace_isolation_survive_the_switch() {
    let mut f = CommandFixture::create(0).await.unwrap();
    for id in 1..=3 {
        f.store
            .execute(f.session, Uuid::now_v7(), create(id, &format!("task {id}")))
            .await
            .unwrap();
    }
    let other = f.store.create_workspace(f.session.principal).await.unwrap();
    f.store
        .execute(other, Uuid::now_v7(), create(1, "private"))
        .await
        .unwrap();
    let q = TaskQuery {
        completed: Some(false),
        ..query(2)
    };
    let mut feed = TaskFeed::new(&f.store, 2).await.unwrap();
    let mut fast = feed.subscribe(&mut f.store, f.session, q).await.unwrap();
    let mut slow = feed.subscribe(&mut f.store, f.session, q).await.unwrap();
    assert!(matches!(
        feed.subscribe(&mut f.store, f.session, q).await,
        Err(TaskFeedError::Limit)
    ));
    let initial = receive(&mut fast, &f.store).await;
    assert_eq!(
        tasks(&initial).iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![Uuid::from_u128(1), Uuid::from_u128(2)]
    );
    f.store
        .execute(
            f.session,
            Uuid::now_v7(),
            Command::Delete {
                id: Uuid::from_u128(1),
                expected_version: 1,
            },
        )
        .await
        .unwrap();
    feed.refresh(&mut f.store).await.unwrap();
    let replacement = receive(&mut fast, &f.store).await;
    assert_eq!(
        tasks(&replacement).iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![Uuid::from_u128(2), Uuid::from_u128(3)]
    );
    for version in 1..=4 {
        f.store
            .execute(
                f.session,
                Uuid::now_v7(),
                Command::Patch {
                    id: Uuid::from_u128(2),
                    expected_version: version,
                    title: Some(format!("latest {version}")),
                    completed: None,
                },
            )
            .await
            .unwrap();
        feed.refresh(&mut f.store).await.unwrap();
    }
    let latest = receive(&mut slow, &f.store).await;
    assert_eq!(latest.cursor.subscription, initial.cursor.subscription);
    assert!(latest.cursor.sequence > replacement.cursor.sequence);
    assert_eq!(tasks(&latest)[0].title, "latest 4");
    let encoded = serde_json::to_value(latest.as_ref()).unwrap();
    assert!(
        encoded["result"]["Snapshot"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r.as_object().unwrap().len() == 3)
    );
    feed.refresh(&mut f.store).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), slow.next(&f.store))
            .await
            .is_err()
    );
    f.store
        .delete_fixture_workspace(other.workspace)
        .await
        .unwrap();
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn permission_revisions_revoke_queued_and_quiet_subscriptions_without_task_writes() {
    let mut f = CommandFixture::create(1).await.unwrap();
    let reader = Session {
        principal: Uuid::now_v7(),
        ..f.session
    };
    let q = query(100);
    let mut feed = TaskFeed::new(&f.store, 3).await.unwrap();
    assert!(matches!(
        feed.subscribe(&mut f.store, reader, q).await,
        Err(TaskFeedError::Authorization(Error::Forbidden))
    ));
    let before = f.store.managed_head(f.session).await.unwrap();
    f.store
        .set_member(reader.workspace, reader.principal, Some(false))
        .await
        .unwrap();
    let granted = f.store.managed_head(f.session).await.unwrap();
    assert_eq!(granted.revision.sequence, before.revision.sequence + 1);
    let mut consumed = feed.subscribe(&mut f.store, reader, q).await.unwrap();
    let mut queued = feed.subscribe(&mut f.store, reader, q).await.unwrap();
    assert_eq!(
        tasks(receive(&mut consumed, &f.store).await.as_ref()).len(),
        1
    );
    f.store
        .set_member(reader.workspace, reader.principal, None)
        .await
        .unwrap();
    assert_eq!(
        f.store
            .managed_head(f.session)
            .await
            .unwrap()
            .revision
            .sequence,
        granted.revision.sequence + 1
    );
    assert_eq!(
        receive(&mut queued, &f.store).await.result,
        TaskResult::Revoked
    );
    feed.refresh(&mut f.store).await.unwrap();
    f.store
        .set_member(reader.workspace, reader.principal, Some(false))
        .await
        .unwrap();
    assert_eq!(
        receive(&mut consumed, &f.store).await.result,
        TaskResult::Revoked
    );
    assert!(matches!(
        consumed.next(&f.store).await,
        Err(TaskFeedError::Closed)
    ));
    let mut fresh = feed.subscribe(&mut f.store, reader, q).await.unwrap();
    assert_eq!(tasks(receive(&mut fresh, &f.store).await.as_ref()).len(), 1);
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn failed_revision_update_rolls_back_permission_change() {
    let mut f = CommandFixture::create(1).await.unwrap();
    let reader = Session {
        principal: Uuid::now_v7(),
        ..f.session
    };
    let before = f.store.managed_head(f.session).await.unwrap();
    let name = format!("fail_revision_{}", Uuid::now_v7().simple());
    f.db.batch_execute(&format!("CREATE FUNCTION _hulladb_task_probe.{name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected revision failure'; END $$; CREATE TRIGGER {name} BEFORE UPDATE ON _hulladb_task_probe.workspaces FOR EACH ROW WHEN (NEW.id='{}'::uuid) EXECUTE FUNCTION _hulladb_task_probe.{name}();", f.session.workspace)).await.unwrap();
    let failed = f
        .store
        .set_member(reader.workspace, reader.principal, Some(false))
        .await;
    f.db.batch_execute(&format!("DROP TRIGGER {name} ON _hulladb_task_probe.workspaces; DROP FUNCTION _hulladb_task_probe.{name}();")).await.unwrap();
    assert!(matches!(failed, Err(Error::Database(_))));
    assert_eq!(f.store.managed_head(f.session).await.unwrap(), before);
    assert!(matches!(
        f.store.managed_head(reader).await,
        Err(Error::Forbidden)
    ));
    f.store
        .set_member(reader.workspace, reader.principal, Some(false))
        .await
        .unwrap();
    assert_eq!(
        f.store
            .managed_head(f.session)
            .await
            .unwrap()
            .revision
            .sequence,
        before.revision.sequence + 1
    );
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn cancelled_delivery_retains_pending_message() {
    let mut f = CommandFixture::create(1).await.unwrap();
    let mut feed = TaskFeed::new(&f.store, 1).await.unwrap();
    let mut sub = feed
        .subscribe(&mut f.store, f.session, query(1))
        .await
        .unwrap();
    let tx = f.db.transaction().await.unwrap();
    tx.batch_execute("LOCK TABLE _hulladb_task_probe.members IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), sub.next(&f.store))
            .await
            .is_err()
    );
    tx.commit().await.unwrap();
    assert_eq!(tasks(receive(&mut sub, &f.store).await.as_ref()).len(), 1);
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn source_reset_rejects_queued_data_before_refresh_and_closes_existing_lifecycles() {
    let mut f = CommandFixture::create(1).await.unwrap();
    let source = f.store.managed_source().await.unwrap();
    let mut feed = TaskFeed::new(&f.store, 2).await.unwrap();
    let mut queued = feed
        .subscribe(&mut f.store, f.session, query(1))
        .await
        .unwrap();
    let mut consumed = feed
        .subscribe(&mut f.store, f.session, query(1))
        .await
        .unwrap();
    receive(&mut consumed, &f.store).await;
    f.db.execute(
        "UPDATE _hulladb_task_probe.metadata SET source=$1 WHERE singleton",
        &[&Uuid::now_v7()],
    )
    .await
    .unwrap();
    let queued_result = receive(&mut queued, &f.store).await;
    let failed = feed.refresh(&mut f.store).await;
    // Restore global fixture metadata before assertions/cleanup. Run this target serially.
    f.db.execute(
        "UPDATE _hulladb_task_probe.metadata SET source=$1 WHERE singleton",
        &[&source],
    )
    .await
    .unwrap();
    assert_eq!(queued_result.result, TaskResult::ResetRequired);
    assert!(matches!(failed, Err(TaskFeedError::SourceMismatch)));
    assert_eq!(
        receive(&mut consumed, &f.store).await.result,
        TaskResult::ResetRequired
    );
    assert!(matches!(
        consumed.next(&f.store).await,
        Err(TaskFeedError::Closed)
    ));
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn revision_regression_requires_a_fresh_lifecycle() {
    let mut f = CommandFixture::create(1).await.unwrap();
    let mut feed = TaskFeed::new(&f.store, 1).await.unwrap();
    let mut sub = feed
        .subscribe(&mut f.store, f.session, query(1))
        .await
        .unwrap();
    receive(&mut sub, &f.store).await;
    f.db.execute(
        "UPDATE _hulladb_task_probe.workspaces SET sequence=0 WHERE id=$1",
        &[&f.session.workspace],
    )
    .await
    .unwrap();
    assert!(matches!(
        feed.refresh(&mut f.store).await,
        Err(TaskFeedError::SourceMismatch)
    ));
    assert_eq!(
        receive(&mut sub, &f.store).await.result,
        TaskResult::ResetRequired
    );
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn authorized_keyset_pages_need_no_full_replica_or_replication_slot() {
    let mut f = CommandFixture::create(100_001).await.unwrap();
    let mut feed = TaskFeed::new(&f.store, 4).await.unwrap();
    for (completed, after, limit) in [
        (None, None, 100),
        (Some(true), Some(Uuid::from_u128(1 << 127)), 50),
        (Some(false), None, 7),
        (None, Some(Uuid::from_u128(u128::MAX)), 1),
    ] {
        let q = TaskQuery {
            completed,
            after,
            limit,
        };
        let expected = f.store.query_snapshot(f.session, q).await.unwrap();
        let mut sub = feed.subscribe(&mut f.store, f.session, q).await.unwrap();
        assert_eq!(
            tasks(receive(&mut sub, &f.store).await.as_ref()),
            expected.tasks
        );
    }
    for limit in [0, 101] {
        assert!(matches!(
            feed.subscribe(&mut f.store, f.session, query(limit)).await,
            Err(TaskFeedError::Query(Error::InvalidInput(_)))
        ));
    }
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL role administration"]
async fn managed_delivery_works_with_a_nonreplication_runtime_role_and_denies_direct_client_dml() {
    let mut f = CommandFixture::create(0).await.unwrap();
    let runtime = format!("managed_runtime_{}", Uuid::now_v7().simple());
    let client_role = format!("managed_client_{}", Uuid::now_v7().simple());
    let password = Uuid::now_v7().to_string();
    // The runtime is trusted server code, not an application client SQL identity.
    f.db.batch_execute(&format!(
        "CREATE ROLE {runtime} LOGIN PASSWORD '{password}' NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION;
         CREATE ROLE {client_role} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION;
         GRANT USAGE ON SCHEMA _hulladb_task_probe TO {runtime},{client_role};
         GRANT SELECT,INSERT,UPDATE,DELETE ON _hulladb_task_probe.tasks,_hulladb_task_probe.workspaces,_hulladb_task_probe.members,_hulladb_task_probe.receipts TO {runtime};
         GRANT SELECT ON _hulladb_task_probe.metadata TO {runtime};"
    )).await.unwrap();
    let mut config: tokio_postgres::Config = f.url.parse().unwrap();
    config.user(&runtime).password(&password);
    let mut store = Store::connect_config(config).await.unwrap();
    store
        .execute(f.session, Uuid::now_v7(), create(1, "managed role"))
        .await
        .unwrap();
    let mut feed = TaskFeed::new(&store, 1).await.unwrap();
    let mut sub = feed
        .subscribe(&mut store, f.session, query(1))
        .await
        .unwrap();
    let message = receive(&mut sub, &store).await;
    let tx = f.db.transaction().await.unwrap();
    tx.batch_execute(&format!("SET LOCAL ROLE {client_role}"))
        .await
        .unwrap();
    let denied = tx
        .execute(
            "UPDATE _hulladb_task_probe.tasks SET title='bypass' WHERE workspace=$1",
            &[&f.session.workspace],
        )
        .await;
    tx.rollback().await.unwrap();
    drop(sub);
    drop(feed);
    drop(store);
    f.db.batch_execute(&format!(
        "DROP OWNED BY {runtime},{client_role}; DROP ROLE {runtime}; DROP ROLE {client_role};"
    ))
    .await
    .unwrap();
    assert_eq!(tasks(&message)[0].title, "managed role");
    assert_eq!(
        denied.unwrap_err().code(),
        Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE)
    );
    f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn contradictory_results_at_one_revision_close_the_old_group() {
    let mut f = CommandFixture::create(1).await.unwrap();
    let mut feed = TaskFeed::new(&f.store, 2).await.unwrap();
    let mut sub = feed
        .subscribe(&mut f.store, f.session, query(1))
        .await
        .unwrap();
    receive(&mut sub, &f.store).await;
    // An administrator bypasses managed tracking. This is unsupported; when a
    // fresh read exposes contradictory coverage, the existing group must close.
    f.db.execute(
        "UPDATE _hulladb_task_probe.tasks SET title='untracked' WHERE workspace=$1",
        &[&f.session.workspace],
    )
    .await
    .unwrap();
    assert!(matches!(
        feed.subscribe(&mut f.store, f.session, query(1)).await,
        Err(TaskFeedError::SourceMismatch)
    ));
    // Another polling tick must not reopen a terminal group.
    feed.refresh(&mut f.store).await.unwrap();
    assert_eq!(
        receive(&mut sub, &f.store).await.result,
        TaskResult::ResetRequired
    );
    assert!(matches!(
        sub.next(&f.store).await,
        Err(TaskFeedError::Closed)
    ));
    f.cleanup().await.unwrap();
}
