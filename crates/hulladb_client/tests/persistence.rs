use std::{path::PathBuf, time::Duration};

use hulladb_client::{
    DeliveryCursor, Error, ReplicaOptions, Session, TaskMessage, TaskQuery, TaskReplica,
    TaskResult, TaskSummary, VisibilityFence,
};
use uuid::Uuid;

struct File(PathBuf);
impl File {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("hulladb-client-{}.sqlite", Uuid::now_v7())))
    }
}
impl Drop for File {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}
fn options() -> ReplicaOptions {
    ReplicaOptions {
        max_chunk_rows: 50,
        max_chunk_bytes: 32 * 1024,
        retained_deliveries_per_query: 2,
        busy_timeout: Duration::from_secs(1),
    }
}
fn session() -> Session {
    Session {
        workspace: Uuid::now_v7(),
        principal: Uuid::now_v7(),
    }
}
fn query() -> TaskQuery {
    TaskQuery {
        completed: None,
        after: None,
        limit: 100,
    }
}
fn task(id: u128, title: &str) -> TaskSummary {
    TaskSummary {
        id: Uuid::from_u128(id),
        title: title.into(),
        completed: false,
    }
}
fn fence(scope: Session, sequence: i64) -> VisibilityFence {
    VisibilityFence {
        source: Uuid::from_u128(999),
        workspace: scope.workspace,
        sequence,
    }
}

fn message(
    session: Session,
    query: TaskQuery,
    subscription: Uuid,
    sequence: i64,
    tasks: Vec<TaskSummary>,
) -> TaskMessage {
    TaskMessage {
        visibility: fence(session, sequence),
        session,
        query,
        cursor: DeliveryCursor {
            subscription,
            sequence,
        },
        result: TaskResult::Snapshot { tasks },
    }
}

#[test]
fn streamed_snapshot_publishes_rows_history_and_cursor_atomically_and_reopens() {
    let file = File::new();
    let scope = session();
    let subscription = Uuid::now_v7();
    let mut client = TaskReplica::open(&file.0, scope, options()).unwrap();
    client.register(query(), subscription).unwrap();
    client
        .apply(&message(
            scope,
            query(),
            subscription,
            1,
            vec![task(1, "old")],
        ))
        .unwrap();
    let observer = TaskReplica::open(&file.0, scope, options()).unwrap();
    {
        let mut writer = client
            .begin_snapshot(
                query(),
                DeliveryCursor {
                    subscription,
                    sequence: 2,
                },
                fence(scope, 2),
            )
            .unwrap();
        writer.append(&[task(2, "part one")]).unwrap();
        assert_eq!(
            observer.read(scope, query()).unwrap().tasks,
            vec![task(1, "old")]
        );
        // Dropping a cancelled/unfinished bootstrap must restore the prior page.
    }
    assert_eq!(client.read(scope, query()).unwrap().cursor.sequence, 1);
    let mut writer = client
        .begin_snapshot(
            query(),
            DeliveryCursor {
                subscription,
                sequence: 3,
            },
            fence(scope, 3),
        )
        .unwrap();
    writer.append(&[task(2, "part one")]).unwrap();
    writer.append(&[task(3, "part two")]).unwrap();
    writer.finish().unwrap();
    assert_eq!(
        observer.read(scope, query()).unwrap().tasks,
        vec![task(2, "part one"), task(3, "part two")]
    );
    drop(client);
    let reopened = TaskReplica::open(&file.0, scope, options()).unwrap();
    assert_eq!(reopened.read(scope, query()).unwrap().cursor.sequence, 3);
    let db = rusqlite::Connection::open(&file.0).unwrap();
    let pending: i64 = db
        .query_row(
            "SELECT count(*) FROM client_events WHERE sequence=2",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        pending, 0,
        "interrupted bootstrap history must roll back too"
    );
}

#[test]
fn late_wrong_scope_malformed_and_conflicting_replays_cannot_replace_coverage() {
    let file = File::new();
    let scope = session();
    let subscription = Uuid::now_v7();
    let mut client = TaskReplica::open(&file.0, scope, options()).unwrap();
    client.register(query(), subscription).unwrap();
    let good = message(scope, query(), subscription, 3, vec![task(1, "good")]);
    client.apply(&good).unwrap();
    client.apply(&good).unwrap();
    assert!(matches!(
        client.apply(&message(scope, query(), subscription, 2, vec![])),
        Err(Error::InvalidCursor)
    ));
    assert!(matches!(
        client.apply(&message(session(), query(), subscription, 4, vec![])),
        Err(Error::ScopeMismatch)
    ));
    assert!(matches!(
        client.apply(&message(
            scope,
            query(),
            subscription,
            3,
            vec![task(1, "different")]
        )),
        Err(Error::Corrupt)
    ));
    assert!(
        client
            .apply(&message(
                scope,
                query(),
                subscription,
                4,
                vec![task(2, "later"), task(1, "earlier")]
            ))
            .is_err()
    );
    assert_eq!(
        client.read(scope, query()).unwrap().tasks,
        vec![task(1, "good")]
    );
    let reset = TaskMessage {
        cursor: DeliveryCursor {
            subscription,
            sequence: 4,
        },
        result: TaskResult::ResetRequired,
        ..good.clone()
    };
    client.apply(&reset).unwrap();
    assert!(matches!(client.apply(&good), Err(Error::Closed)));
    let replacement = Uuid::now_v7();
    client.register(query(), replacement).unwrap();
    client
        .apply(&message(scope, query(), replacement, 1, vec![]))
        .unwrap();
    assert!(matches!(client.apply(&good), Err(Error::InvalidCursor)));
    assert!(client.read(scope, query()).unwrap().tasks.is_empty());
}

#[test]
fn revocation_erases_all_scope_data_and_event_payloads_and_closes_reopened_database() {
    let file = File::new();
    let scope = session();
    let subscription = Uuid::now_v7();
    let mut client = TaskReplica::open(&file.0, scope, options()).unwrap();
    for query in [
        query(),
        TaskQuery {
            limit: 1,
            ..query()
        },
    ] {
        client.register(query, subscription).unwrap();
        client
            .apply(&message(
                scope,
                query,
                subscription,
                1,
                vec![task(1, "private")],
            ))
            .unwrap();
    }
    let revoked = TaskMessage {
        result: TaskResult::Revoked,
        ..message(scope, query(), subscription, 2, vec![])
    };
    client.apply(&revoked).unwrap();
    assert!(matches!(client.read(scope, query()), Err(Error::Closed)));
    assert!(matches!(
        client.register(query(), Uuid::now_v7()),
        Err(Error::Closed)
    ));
    drop(client);
    assert!(matches!(
        TaskReplica::open(&file.0, scope, options()),
        Err(Error::Closed)
    ));
    let db = rusqlite::Connection::open(&file.0).unwrap();
    let remaining: i64 = db
        .query_row(
            "SELECT (SELECT count(*) FROM client_tasks)+(SELECT count(*) FROM client_events)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0);
    assert!(matches!(
        TaskReplica::open(&file.0, session(), options()),
        Err(Error::ScopeMismatch)
    ));
}

#[test]
fn failed_chunk_poisoning_and_storage_commit_failure_preserve_previous_page() {
    let file = File::new();
    let scope = session();
    let subscription = Uuid::now_v7();
    let mut client = TaskReplica::open(
        &file.0,
        scope,
        ReplicaOptions {
            max_chunk_rows: 1,
            ..options()
        },
    )
    .unwrap();
    client.register(query(), subscription).unwrap();
    client
        .apply(&message(
            scope,
            query(),
            subscription,
            1,
            vec![task(1, "old")],
        ))
        .unwrap();
    let mut writer = client
        .begin_snapshot(
            query(),
            DeliveryCursor {
                subscription,
                sequence: 2,
            },
            fence(scope, 2),
        )
        .unwrap();
    writer.append(&[task(2, "valid prefix")]).unwrap();
    assert!(matches!(
        writer.append(&[task(3, "too many"), task(4, "rows")]),
        Err(Error::Budget {
            resource: "chunk rows",
            ..
        })
    ));
    assert!(writer.finish().is_err());
    assert_eq!(
        client.read(scope, query()).unwrap().tasks,
        vec![task(1, "old")]
    );
    let db = rusqlite::Connection::open(&file.0).unwrap();
    db.execute_batch("CREATE TRIGGER reject_cursor BEFORE UPDATE ON client_queries BEGIN SELECT RAISE(ABORT,'cursor write failed'); END;").unwrap();
    assert!(
        client
            .apply(&message(
                scope,
                query(),
                subscription,
                2,
                vec![task(2, "new")]
            ))
            .is_err()
    );
    assert_eq!(client.read(scope, query()).unwrap().cursor.sequence, 1);
    assert_eq!(
        client.read(scope, query()).unwrap().tasks,
        vec![task(1, "old")]
    );
    db.execute_batch("DROP TRIGGER reject_cursor;").unwrap();
    client
        .apply(&message(
            scope,
            query(),
            subscription,
            2,
            vec![task(2, "new")],
        ))
        .unwrap();
}

#[test]
fn byte_budget_and_history_retention_do_not_discard_current_rows() {
    let file = File::new();
    let scope = session();
    let subscription = Uuid::now_v7();
    let mut client = TaskReplica::open(
        &file.0,
        scope,
        ReplicaOptions {
            max_chunk_bytes: 256,
            ..options()
        },
    )
    .unwrap();
    client.register(query(), subscription).unwrap();
    for sequence in 1..=5 {
        client
            .apply(&message(
                scope,
                query(),
                subscription,
                sequence,
                vec![task(1, &format!("state {sequence}"))],
            ))
            .unwrap();
    }
    assert!(matches!(
        client.apply(&message(
            scope,
            query(),
            subscription,
            6,
            vec![task(1, &"x".repeat(300))]
        )),
        Err(Error::Budget {
            resource: "chunk bytes",
            ..
        })
    ));
    assert_eq!(
        client.read(scope, query()).unwrap().tasks,
        vec![task(1, "state 5")]
    );
    let db = rusqlite::Connection::open(&file.0).unwrap();
    let retained: i64 = db
        .query_row(
            "SELECT count(DISTINCT sequence) FROM client_events",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(retained, 2);
}

#[test]
fn stored_working_set_can_exceed_old_row_and_payload_ceilings_with_bounded_chunks() {
    let file = File::new();
    let scope = session();
    let subscription = Uuid::now_v7();
    let mut client = TaskReplica::open(
        &file.0,
        scope,
        ReplicaOptions {
            retained_deliveries_per_query: 0,
            ..options()
        },
    )
    .unwrap();
    let title = "x".repeat(192);
    // Each page retains its own exact coverage. The database grows without ever
    // building the whole working set in RAM or accepting an oversized query.
    for page in 0..1001_u128 {
        let query = TaskQuery {
            after: Some(Uuid::from_u128(page * 100)),
            ..query()
        };
        client.register(query, subscription).unwrap();
        let mut writer = client
            .begin_snapshot(
                query,
                DeliveryCursor {
                    subscription,
                    sequence: 1,
                },
                fence(scope, 1),
            )
            .unwrap();
        for half in 0..2 {
            let chunk = (1..=50)
                .map(|i| task(page * 100 + half * 50 + i, &title))
                .collect::<Vec<_>>();
            writer.append(&chunk).unwrap();
        }
        writer.finish().unwrap();
    }
    let db = rusqlite::Connection::open(&file.0).unwrap();
    let (rows, bytes): (i64, i64) = db
        .query_row(
            "SELECT count(*),sum(length(CAST(title AS BLOB))) FROM client_tasks",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(rows > 100_000 && bytes > 16 * 1024 * 1024);
    drop(client);
    let client = TaskReplica::open(&file.0, scope, options()).unwrap();
    assert_eq!(
        client
            .read(
                scope,
                TaskQuery {
                    after: Some(Uuid::from_u128(100_000)),
                    ..query()
                }
            )
            .unwrap()
            .tasks
            .len(),
        100
    );
}

#[test]
fn visibility_advances_only_with_complete_publication_and_survives_reopen() {
    let file = File::new();
    let scope = session();
    let subscription = Uuid::now_v7();
    let q = query();
    let mut client = TaskReplica::open(&file.0, scope, options()).unwrap();
    client.register(q, subscription).unwrap();
    let target = fence(scope, 9);
    assert!(matches!(
        client.is_visible(scope, q, target),
        Err(Error::NotCovered)
    ));
    client
        .apply(&message(scope, q, subscription, 1, vec![]))
        .unwrap();
    let observer = TaskReplica::open(&file.0, scope, options()).unwrap();
    assert!(!client.is_visible(scope, q, target).unwrap());
    {
        let mut writer = client
            .begin_snapshot(
                q,
                DeliveryCursor {
                    subscription,
                    sequence: 3,
                },
                target,
            )
            .unwrap();
        writer.append(&[]).unwrap();
        assert!(!observer.is_visible(scope, q, target).unwrap());
        // Cancellation cannot publish progress ahead of the corresponding page.
    }
    assert!(!client.is_visible(scope, q, target).unwrap());
    let db = rusqlite::Connection::open(&file.0).unwrap();
    db.execute_batch("CREATE TRIGGER reject_progress BEFORE UPDATE ON client_queries BEGIN SELECT RAISE(ABORT,'progress write failed'); END;").unwrap();
    let replacement = TaskMessage {
        visibility: target,
        ..message(scope, q, subscription, 3, vec![])
    };
    assert!(client.apply(&replacement).is_err());
    assert!(!observer.is_visible(scope, q, target).unwrap());
    db.execute_batch("DROP TRIGGER reject_progress;").unwrap();
    client.apply(&replacement).unwrap();
    assert!(observer.is_visible(scope, q, target).unwrap());
    // Coverage for another exact query is independent, even in this workspace.
    let other = TaskQuery { limit: 1, ..q };
    client.register(other, Uuid::now_v7()).unwrap();
    assert!(matches!(
        client.is_visible(scope, other, target),
        Err(Error::NotCovered)
    ));
    drop(client);
    let client = TaskReplica::open(&file.0, scope, options()).unwrap();
    assert!(client.is_visible(scope, q, fence(scope, 8)).unwrap());
    assert!(!client.is_visible(scope, q, fence(scope, 10)).unwrap());
}

#[test]
fn visibility_rejects_foreign_sources_regressions_and_conflicting_replays() {
    let file = File::new();
    let scope = session();
    let subscription = Uuid::now_v7();
    let q = query();
    let mut client = TaskReplica::open(&file.0, scope, options()).unwrap();
    client.register(q, subscription).unwrap();
    let initial = message(scope, q, subscription, 3, vec![task(1, "stable")]);
    client.apply(&initial).unwrap();
    let foreign = VisibilityFence {
        source: Uuid::now_v7(),
        ..initial.visibility
    };
    assert!(matches!(
        client.is_visible(scope, q, foreign),
        Err(Error::InvalidVisibility)
    ));
    for visibility in [foreign, fence(scope, 2)] {
        assert!(matches!(
            client.apply(&TaskMessage {
                visibility,
                cursor: DeliveryCursor {
                    sequence: 4,
                    ..initial.cursor
                },
                ..initial.clone()
            }),
            Err(Error::InvalidVisibility)
        ));
    }
    assert!(matches!(
        client.apply(&TaskMessage {
            visibility: fence(scope, 4),
            ..initial.clone()
        }),
        Err(Error::Corrupt)
    ));
    for visibility in [fence(session(), 3), fence(scope, -1)] {
        assert!(client.is_visible(scope, q, visibility).is_err());
        assert!(
            client
                .apply(&TaskMessage {
                    visibility,
                    cursor: DeliveryCursor {
                        sequence: 4,
                        ..initial.cursor
                    },
                    ..initial.clone()
                })
                .is_err()
        );
    }
    assert_eq!(
        client.read(scope, q).unwrap().visibility,
        initial.visibility
    );
    client
        .apply(&TaskMessage {
            result: TaskResult::ResetRequired,
            ..initial
        })
        .unwrap();
    assert!(matches!(
        client.is_visible(scope, q, fence(scope, 3)),
        Err(Error::Closed)
    ));
    let replacement = Uuid::now_v7();
    client.register(q, replacement).unwrap();
    assert!(matches!(
        client.is_visible(scope, q, foreign),
        Err(Error::NotCovered)
    ));
    client
        .apply(&TaskMessage {
            visibility: foreign,
            ..message(scope, q, replacement, 1, vec![])
        })
        .unwrap();
    assert!(client.is_visible(scope, q, foreign).unwrap());
    assert!(matches!(
        client.is_visible(scope, q, fence(scope, 1)),
        Err(Error::InvalidVisibility)
    ));
}
