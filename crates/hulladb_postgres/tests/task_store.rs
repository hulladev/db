use hulladb_postgres::{Command, Error, QueryCache, QueryStatus, Session, Store, TaskQuery, Uuid};

fn url() -> String {
    std::env::var("HULLADB_TEST_URL")
        .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into())
}

async fn fixture() -> (Store, Session) {
    let mut store = Store::connect(&url()).await.unwrap();
    store.initialize().await.unwrap();
    let session = store.create_workspace(Uuid::now_v7()).await.unwrap();
    (store, session)
}

fn all_tasks() -> TaskQuery {
    TaskQuery {
        completed: None,
        after: None,
        limit: 100,
    }
}

fn create(id: Uuid) -> Command {
    Command::Create {
        id,
        title: "Task".into(),
        completed: false,
    }
}

fn patch(id: Uuid, version: i64) -> Command {
    Command::Patch {
        id,
        expected_version: version,
        title: None,
        completed: Some(true),
    }
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn receipts_recover_commits_and_reject_changed_payloads_without_extra_effects() {
    let (mut store, session) = fixture().await;
    let request = Uuid::now_v7();
    let id = Uuid::now_v7();
    let receipt = store.execute(session, request, create(id)).await.unwrap();
    // Simulate a lost response by recovering via a new database connection.
    let mut reconnected = Store::connect(&url()).await.unwrap();
    assert_eq!(
        reconnected.receipt(session, request).await.unwrap(),
        Some(receipt.clone())
    );
    assert_eq!(
        reconnected
            .execute(session, request, create(id))
            .await
            .unwrap(),
        receipt
    );
    assert!(matches!(
        store
            .execute(session, request, create(Uuid::now_v7()))
            .await,
        Err(Error::RequestIdReused)
    ));
    let snapshot = store.query_snapshot(session, all_tasks()).await.unwrap();
    assert_eq!(snapshot.revision.sequence, 1);
    assert_eq!(snapshot.tasks.len(), 1);
    store
        .delete_fixture_workspace(session.workspace)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn concurrent_expected_version_updates_have_one_winner_and_atomic_rollback() {
    let (mut store, session) = fixture().await;
    let id = Uuid::now_v7();
    store
        .execute(session, Uuid::now_v7(), create(id))
        .await
        .unwrap();
    let mut other = Store::connect(&url()).await.unwrap();
    let request_a = Uuid::now_v7();
    let request_b = Uuid::now_v7();
    let (a, b) = tokio::join!(
        store.execute(session, request_a, patch(id, 1)),
        other.execute(session, request_b, patch(id, 1)),
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let rejected = if a.is_err() {
        assert!(matches!(a, Err(Error::Conflict)));
        request_a
    } else {
        assert!(matches!(b, Err(Error::Conflict)));
        request_b
    };
    assert!(store.receipt(session, rejected).await.unwrap().is_none());
    let snapshot = store.query_snapshot(session, all_tasks()).await.unwrap();
    assert_eq!(snapshot.revision.sequence, 2);
    assert_eq!(
        store
            .list_tasks(session, None, None, 100)
            .await
            .unwrap()
            .tasks[0]
            .version,
        2
    );
    assert!(snapshot.tasks[0].completed);
    store
        .delete_fixture_workspace(session.workspace)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn failed_receipt_write_rolls_back_task_revision_and_receipt() {
    let (mut store, session) = fixture().await;
    let id = Uuid::now_v7();
    store
        .execute(session, Uuid::now_v7(), create(id))
        .await
        .unwrap();
    let before = store.query_snapshot(session, all_tasks()).await.unwrap();
    let request = Uuid::now_v7();
    let (admin, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    // Generated UUID identifiers and literals are trusted, never user input.
    let name = format!("fail_receipt_{}", Uuid::now_v7().simple());
    admin.batch_execute(&format!(
        "CREATE FUNCTION _hulladb_task_probe.{name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected receipt failure'; END $$;
         CREATE TRIGGER {name} BEFORE INSERT ON _hulladb_task_probe.receipts FOR EACH ROW WHEN (NEW.workspace = '{}'::uuid) EXECUTE FUNCTION _hulladb_task_probe.{name}();",
        session.workspace
    )).await.unwrap();
    let failed = store.execute(session, request, patch(id, 1)).await;
    // Remove fault injection before asserting, even if the tested operation unexpectedly succeeds.
    admin.batch_execute(&format!(
        "DROP TRIGGER {name} ON _hulladb_task_probe.receipts; DROP FUNCTION _hulladb_task_probe.{name}();"
    )).await.unwrap();
    connection.abort();
    assert!(matches!(failed, Err(Error::Database(_))));
    let after = store.query_snapshot(session, all_tasks()).await.unwrap();
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.tasks, before.tasks);
    assert!(store.receipt(session, request).await.unwrap().is_none());
    let retried = store.execute(session, request, patch(id, 1)).await.unwrap();
    assert_eq!(retried.revision.sequence, 2);
    store
        .delete_fixture_workspace(session.workspace)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn duplicate_concurrent_submission_commits_once() {
    let (mut store, session) = fixture().await;
    let mut other = Store::connect(&url()).await.unwrap();
    let request = Uuid::now_v7();
    let id = Uuid::now_v7();
    let (a, b) = tokio::join!(
        store.execute(session, request, create(id)),
        other.execute(session, request, create(id))
    );
    assert_eq!(a.unwrap(), b.unwrap());
    assert_eq!(
        store
            .query_snapshot(session, all_tasks())
            .await
            .unwrap()
            .revision
            .sequence,
        1
    );
    store
        .delete_fixture_workspace(session.workspace)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn deletion_does_not_resurrect_under_a_stale_update() {
    let (mut store, session) = fixture().await;
    let id = Uuid::now_v7();
    store
        .execute(session, Uuid::now_v7(), create(id))
        .await
        .unwrap();
    store
        .execute(
            session,
            Uuid::now_v7(),
            Command::Delete {
                id,
                expected_version: 1,
            },
        )
        .await
        .unwrap();
    let request = Uuid::now_v7();
    assert!(matches!(
        store.execute(session, request, patch(id, 1)).await,
        Err(Error::NotFound)
    ));
    assert!(store.receipt(session, request).await.unwrap().is_none());
    let snapshot = store.query_snapshot(session, all_tasks()).await.unwrap();
    assert!(snapshot.tasks.is_empty());
    assert_eq!(snapshot.revision.sequence, 2);
    store
        .delete_fixture_workspace(session.workspace)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn authorization_is_enforced_for_queries_mutations_and_receipts() {
    let (mut store, session) = fixture().await;
    let stranger = Session {
        principal: Uuid::now_v7(),
        ..session
    };
    let request = Uuid::now_v7();
    store
        .execute(session, request, create(Uuid::now_v7()))
        .await
        .unwrap();
    assert!(matches!(
        store.list_tasks(stranger, None, None, 10).await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        store.query_snapshot(stranger, all_tasks()).await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        store.receipt(stranger, request).await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        store
            .execute(stranger, Uuid::now_v7(), create(Uuid::now_v7()))
            .await,
        Err(Error::Forbidden)
    ));
    store
        .set_member(session.workspace, stranger.principal, Some(false))
        .await
        .unwrap();
    assert_eq!(
        store
            .query_snapshot(stranger, all_tasks())
            .await
            .unwrap()
            .tasks
            .len(),
        1
    );
    assert!(matches!(
        store
            .execute(stranger, Uuid::now_v7(), create(Uuid::now_v7()))
            .await,
        Err(Error::Forbidden)
    ));
    assert!(store.receipt(stranger, request).await.unwrap().is_none()); // Principal-scoped receipts.
    store
        .set_member(session.workspace, stranger.principal, None)
        .await
        .unwrap();
    store
        .delete_fixture_workspace(session.workspace)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn keyset_pages_are_ordered_bounded_and_workspace_scoped() {
    let (mut store, session) = fixture().await;
    store.seed_fixture(session, 37).await.unwrap();
    let other = store.create_workspace(session.principal).await.unwrap();
    store
        .execute(other, Uuid::now_v7(), create(Uuid::now_v7()))
        .await
        .unwrap();
    for completed in [None, Some(false), Some(true)] {
        let mut after = None;
        let mut ids = Vec::new();
        loop {
            let page = store
                .list_tasks(session, completed, after, 7)
                .await
                .unwrap();
            assert!(page.tasks.len() <= 7);
            assert!(
                page.tasks
                    .iter()
                    .all(|task| completed.is_none_or(|done| done == task.completed))
            );
            ids.extend(page.tasks.iter().map(|task| task.id));
            after = page.next;
            if after.is_none() {
                break;
            }
        }
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(
            ids.len(),
            match completed {
                None => 37,
                Some(true) => 18,
                Some(false) => 19,
            }
        );
    }
    assert!(matches!(
        store.list_tasks(session, None, None, 0).await,
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        store.list_tasks(session, None, None, 101).await,
        Err(Error::InvalidInput(_))
    ));
    store
        .delete_fixture_workspace(session.workspace)
        .await
        .unwrap();
    store
        .delete_fixture_workspace(other.workspace)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn invalid_commands_and_identity_conflicts_leave_no_receipt_or_revision() {
    let (mut store, session) = fixture().await;
    let id = Uuid::now_v7();
    store
        .execute(session, Uuid::now_v7(), create(id))
        .await
        .unwrap();
    for command in [
        Command::Create {
            id: Uuid::now_v7(),
            title: "bad\0title".into(),
            completed: false,
        },
        Command::Patch {
            id,
            expected_version: 1,
            title: None,
            completed: None,
        },
        Command::Delete {
            id,
            expected_version: 0,
        },
        create(id),
    ] {
        let request = Uuid::now_v7();
        assert!(store.execute(session, request, command).await.is_err());
        assert!(store.receipt(session, request).await.unwrap().is_none());
    }
    assert_eq!(
        store
            .query_snapshot(session, all_tasks())
            .await
            .unwrap()
            .revision
            .sequence,
        1
    );
    store
        .delete_fixture_workspace(session.workspace)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn bounded_summary_coverage_refills_top_n_and_clears_revoked_data() {
    let (mut store, session) = fixture().await;
    store.seed_fixture(session, 1_000).await.unwrap();

    let query = TaskQuery {
        completed: Some(false),
        after: None,
        limit: 2,
    };
    let first_three = store
        .list_tasks(session, Some(false), None, 3)
        .await
        .unwrap()
        .tasks;
    let initial = store.query_snapshot(session, query).await.unwrap();
    assert_eq!(initial.tasks.len(), 2);
    assert_eq!(
        initial.tasks.iter().map(|task| task.id).collect::<Vec<_>>(),
        first_three[..2]
            .iter()
            .map(|task| task.id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        initial.tasks[0]
            .values()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["completed", "id", "title"]
    );
    let mut cache = QueryCache::new(session, query).unwrap();
    cache.install(initial).unwrap();
    let receipt = store
        .execute(
            session,
            Uuid::now_v7(),
            Command::Delete {
                id: first_three[0].id,
                expected_version: 1,
            },
        )
        .await
        .unwrap();
    cache.observe(receipt.revision).unwrap();
    assert_eq!(
        cache.read(session, query).unwrap().status,
        QueryStatus::Stale
    );
    assert_eq!(
        cache.read(session, query).unwrap().tasks[0].id,
        first_three[0].id
    );
    cache.begin_refresh().unwrap();
    cache
        .install(store.query_snapshot(session, query).await.unwrap())
        .unwrap();
    let view = cache.read(session, query).unwrap();
    assert_eq!(view.status, QueryStatus::Ready);
    assert_eq!(view.complete_at.unwrap(), receipt.revision);
    assert_eq!(
        view.tasks.iter().map(|task| task.id).collect::<Vec<_>>(),
        first_three[1..]
            .iter()
            .map(|task| task.id)
            .collect::<Vec<_>>()
    );
    let after = TaskQuery {
        after: Some(first_three[1].id),
        ..query
    };
    let page = store.query_snapshot(session, after).await.unwrap();
    assert_eq!(page.tasks[0].id, first_three[2].id);
    assert!(matches!(
        cache.read(session, after),
        Err(Error::QueryNotCovered)
    ));
    let stranger = Session {
        principal: Uuid::now_v7(),
        ..session
    };
    assert!(matches!(
        store.query_snapshot(stranger, query).await,
        Err(Error::Forbidden)
    ));
    store
        .set_member(session.workspace, session.principal, None)
        .await
        .unwrap();
    let forbidden = store.query_snapshot(session, query).await.unwrap_err();
    assert!(matches!(forbidden, Error::Forbidden));
    cache.fail(&forbidden);
    assert!(cache.read(session, query).unwrap().tasks.is_empty());
    store
        .delete_fixture_workspace(session.workspace)
        .await
        .unwrap();
}
