use hulladb_postgres::{
    Error, QueryCache, QueryRevision, QuerySnapshot, QueryStatus, Session, TaskQuery, TaskSummary,
    Uuid,
};

fn query() -> TaskQuery {
    TaskQuery {
        completed: Some(false),
        after: None,
        limit: 2,
    }
}
fn session() -> Session {
    Session {
        workspace: Uuid::now_v7(),
        principal: Uuid::now_v7(),
    }
}
fn snapshot(session: Session, sequence: i64) -> QuerySnapshot {
    QuerySnapshot {
        session,
        query: query(),
        revision: QueryRevision {
            workspace: session.workspace,
            generation: 2,
            sequence,
        },
        tasks: vec![TaskSummary {
            id: Uuid::from_u128(1),
            title: "first".into(),
            completed: false,
        }],
    }
}

#[test]
fn loading_is_distinct_from_complete_empty_and_coverage_is_exact() {
    let session = session();
    let mut cache = QueryCache::new(session, query()).unwrap();
    let view = cache.read(session, query()).unwrap();
    assert!(view.tasks.is_empty());
    assert_eq!(view.complete_at, None);
    assert_eq!(view.status, QueryStatus::Loading);
    let mut empty = snapshot(session, 0);
    empty.tasks.clear();
    cache.install(empty).unwrap();
    assert_eq!(
        cache
            .read(session, query())
            .unwrap()
            .complete_at
            .unwrap()
            .sequence,
        0
    );
    assert_eq!(
        cache.read(session, query()).unwrap().status,
        QueryStatus::Ready
    );
    for different in [
        TaskQuery {
            limit: 1,
            ..query()
        },
        TaskQuery {
            completed: None,
            ..query()
        },
        TaskQuery {
            after: Some(Uuid::from_u128(1)),
            ..query()
        },
    ] {
        assert!(matches!(
            cache.read(session, different),
            Err(Error::QueryNotCovered)
        ));
    }
}

#[test]
fn delayed_responses_do_not_regress_data_or_claim_a_known_newer_revision() {
    let session = session();
    let mut cache = QueryCache::new(session, query()).unwrap();
    cache.install(snapshot(session, 1)).unwrap();
    cache.observe(snapshot(session, 3).revision).unwrap();
    let view = cache.read(session, query()).unwrap();
    assert_eq!(view.status, QueryStatus::Stale);
    assert_eq!(view.complete_at.unwrap().sequence, 1);
    cache.begin_refresh().unwrap();
    cache.install(snapshot(session, 2)).unwrap();
    assert_eq!(
        cache.read(session, query()).unwrap().status,
        QueryStatus::Stale
    );
    cache.install(snapshot(session, 3)).unwrap();
    assert_eq!(
        cache.read(session, query()).unwrap().status,
        QueryStatus::Ready
    );
    assert!(matches!(
        cache.install(snapshot(session, 2)),
        Err(Error::InvalidQueryRevision)
    ));
    cache.observe(snapshot(session, 1).revision).unwrap();
    assert_eq!(
        cache
            .read(session, query())
            .unwrap()
            .complete_at
            .unwrap()
            .sequence,
        3
    );
    assert_eq!(
        cache.read(session, query()).unwrap().status,
        QueryStatus::Ready
    );
}

#[test]
fn wrong_scopes_queries_and_malformed_results_never_publish() {
    let session = session();
    let mut cache = QueryCache::new(session, query()).unwrap();
    cache.install(snapshot(session, 1)).unwrap();
    let mut invalid = Vec::new();
    let mut other = snapshot(session, 2);
    other.session.principal = Uuid::now_v7();
    invalid.push(other);
    let mut other = snapshot(session, 2);
    other.session.workspace = Uuid::now_v7();
    invalid.push(other);
    let mut other = snapshot(session, 2);
    other.query.limit = 1;
    invalid.push(other);
    let mut other = snapshot(session, 2);
    other.revision.generation = 3;
    invalid.push(other);
    let mut other = snapshot(session, 2);
    other.revision.workspace = Uuid::now_v7();
    invalid.push(other);
    let mut other = snapshot(session, 2);
    other.tasks[0].completed = true;
    invalid.push(other);
    let mut other = snapshot(session, 2);
    other.tasks[0].title.clear();
    invalid.push(other);
    let mut other = snapshot(session, 2);
    other.tasks.push(other.tasks[0].clone());
    invalid.push(other);
    let mut other = snapshot(session, 2);
    other.tasks = vec![other.tasks[0].clone(); 3];
    invalid.push(other);
    let mut other = snapshot(session, 1);
    other.tasks[0].title = "contradictory same-revision data".into();
    invalid.push(other);
    for candidate in invalid {
        assert!(cache.install(candidate).is_err());
        let view = cache.read(session, query()).unwrap();
        assert_eq!(view.complete_at.unwrap().sequence, 1);
        assert_eq!(view.tasks[0].title, "first");
        assert_eq!(view.status, QueryStatus::Ready);
    }
    let bounded = TaskQuery {
        after: Some(Uuid::from_u128(1)),
        ..query()
    };
    let mut cache = QueryCache::new(session, bounded).unwrap();
    let mut before = snapshot(session, 1);
    before.query = bounded;
    assert!(matches!(cache.install(before), Err(Error::CorruptData)));
    for other in [
        Session {
            principal: Uuid::now_v7(),
            ..session
        },
        Session {
            workspace: Uuid::now_v7(),
            ..session
        },
    ] {
        assert!(matches!(cache.read(other, bounded), Err(Error::Forbidden)));
    }
}

#[test]
fn connectivity_and_errors_preserve_historical_coverage_but_revocation_erases_it() {
    let session = session();
    let mut cache = QueryCache::new(session, query()).unwrap();
    cache.install(snapshot(session, 1)).unwrap();
    cache.disconnect();
    cache.install(snapshot(session, 2)).unwrap();
    assert_eq!(
        cache.read(session, query()).unwrap().status,
        QueryStatus::Disconnected
    );
    cache.fail(&Error::CommitOutcomeUnknown);
    assert_eq!(cache.read(session, query()).unwrap().tasks.len(), 1);
    assert_eq!(
        cache
            .read(session, query())
            .unwrap()
            .complete_at
            .unwrap()
            .sequence,
        2
    );
    cache.fail(&Error::Forbidden);
    let view = cache.read(session, query()).unwrap();
    assert!(view.tasks.is_empty());
    assert_eq!(view.complete_at, None);
    assert_eq!(view.status, QueryStatus::Failed);
    assert!(matches!(
        cache.install(snapshot(session, 3)),
        Err(Error::Forbidden)
    ));
    assert!(matches!(cache.begin_refresh(), Err(Error::Forbidden)));
}

#[test]
fn invalid_limits_and_extra_projection_fields_are_rejected() {
    let session = session();
    for limit in [0, 101] {
        assert!(matches!(
            QueryCache::new(session, TaskQuery { limit, ..query() }),
            Err(Error::InvalidInput(_))
        ));
    }
    let extra = serde_json::json!({"id": Uuid::from_u128(1), "title": "first", "completed": false, "version": 1});
    assert!(serde_json::from_value::<TaskSummary>(extra).is_err());
}
