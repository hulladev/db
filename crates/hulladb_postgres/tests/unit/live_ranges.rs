use super::*;
use crate::managed::{ManagedSchema, ManagedStore};
use hulladb_schema::{FieldDefinition, ScalarType, SchemaDeclaration, TableDeclaration};
use std::sync::Mutex;
use tokio_postgres::IsolationLevel;
use uuid::Uuid;

fn url() -> String {
    std::env::var("HULLADB_TEST_URL")
        .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into())
}
async fn fixture() -> (ManagedStore, Uuid) {
    let mut rank = FieldDefinition::new("rank", ScalarType::Int64);
    rank.nullable = true;
    let schema = SchemaDeclaration::new(vec![TableDeclaration::new(
        "items",
        vec![
            FieldDefinition::new("id", ScalarType::Int64),
            rank,
            FieldDefinition::new("group", ScalarType::String),
            FieldDefinition::new("text", ScalarType::String),
        ],
    )])
    .normalize()
    .unwrap();
    let schema = ManagedSchema::new(format!("ranges_{}", Uuid::now_v7().simple()), schema).unwrap();
    let mut store = ManagedStore::connect(&url(), schema).await.unwrap();
    store.initialize().await.unwrap();
    let scope = Uuid::now_v7();
    store.create_scope(scope).await.unwrap();
    (store, scope)
}
async fn insert(store: &ManagedStore, scope: Uuid, id: i64, rank: i64, text: &str) {
    store
        .client
        .execute(
            &format!(
                "INSERT INTO {} (_scope,_version,f0,f1,f2,f3) VALUES ($1,1,$2,$3,'a',$4)",
                store.schema.relation("t0")
            ),
            &[&scope, &id, &rank, &text],
        )
        .await
        .unwrap();
}
fn spec(direction: super::super::Direction) -> PageQuery {
    PageQuery {
        table: "items".into(),
        projection: None,
        filters: vec![super::super::Predicate::Compare {
            transform: None,
            field: "group".into(),
            operator: "=".into(),
            operand: super::super::Operand::Value {
                value: serde_json::json!("a"),
            },
            case_sensitive: None,
        }],
        order: vec![super::super::Order {
            field: "rank".into(),
            direction,
        }],
    }
}
fn ids(rows: &[Row]) -> Vec<i64> {
    rows.iter()
        .map(|row| match row.values["id"] {
            Value::Int64(id) => id,
            _ => panic!("wrong key"),
        })
        .collect()
}
async fn page(
    store: &mut ManagedStore,
    scope: Uuid,
    query: &PageQuery,
    limit: u16,
    cursor: Option<&PageCursor>,
    budget: PageBudget,
) -> Result<RangePage, Error> {
    let tx = store
        .client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .await?;
    let reads = Mutex::new(Default::default());
    let read_budget = Mutex::new(super::super::db::ReadBudget::default());
    let db = ReadDb {
        tx: &tx,
        schema: &store.schema,
        scope,
        reads: &reads,
        read_budget: &read_budget,
    };
    let page = db.page(query, limit, cursor, budget).await?;
    assert!(reads.lock().unwrap().contains("items"));
    tx.commit().await?;
    Ok(page)
}
async fn refresh(
    store: &mut ManagedStore,
    scope: Uuid,
    ranges: &[PageRange],
    budget: PageBudget,
) -> Result<Vec<RangePage>, Error> {
    let tx = store
        .client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .await?;
    let reads = Mutex::new(Default::default());
    let read_budget = Mutex::new(super::super::db::ReadBudget::default());
    let db = ReadDb {
        tx: &tx,
        schema: &store.schema,
        scope,
        reads: &reads,
        read_budget: &read_budget,
    };
    let mut pages = Vec::new();
    for range in ranges {
        pages.push(db.refresh_page(range, budget).await?);
    }
    tx.commit().await?;
    Ok(pages)
}
async fn cleanup(store: &ManagedStore) {
    store
        .client
        .batch_execute(&format!(
            "DROP SCHEMA \"{}\" CASCADE",
            store.schema.namespace()
        ))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires PostgreSQL; stable live-range proof"]
async fn tied_ordering_row_moves_and_deletions_preserve_loaded_ranges() {
    let (mut store, scope) = fixture().await;
    for id in 1..=6 {
        insert(&store, scope, id, (id + 1) / 2, "x").await;
    }
    let first = page(
        &mut store,
        scope,
        &spec(super::super::Direction::Asc),
        2,
        None,
        PageBudget::default(),
    )
    .await
    .unwrap();
    let second = page(
        &mut store,
        scope,
        &spec(super::super::Direction::Asc),
        2,
        first.cursor.as_ref(),
        PageBudget::default(),
    )
    .await
    .unwrap();
    assert_eq!(ids(&first.rows), [1, 2]);
    assert_eq!(ids(&second.rows), [3, 4]);
    let ranges = [first.range, second.range];
    store
        .client
        .execute(
            &format!(
                "UPDATE {} SET f1=2 WHERE _scope=$1 AND f0=1",
                store.schema.relation("t0")
            ),
            &[&scope],
        )
        .await
        .unwrap();
    store
        .client
        .execute(
            &format!(
                "DELETE FROM {} WHERE _scope=$1 AND f0=3",
                store.schema.relation("t0")
            ),
            &[&scope],
        )
        .await
        .unwrap();
    let pages = refresh(&mut store, scope, &ranges, PageBudget::default())
        .await
        .unwrap();
    assert_eq!(ids(&pages[0].rows), [2]);
    assert_eq!(ids(&pages[1].rows), [1, 4]);
    assert_eq!(pages[0].range, ranges[0]);
    assert_eq!(pages[1].range, ranges[1]);
    assert_eq!(
        pages
            .iter()
            .flat_map(|page| ids(&page.rows))
            .collect::<Vec<_>>(),
        [2, 1, 4]
    );
    let descending = page(
        &mut store,
        scope,
        &spec(super::super::Direction::Desc),
        2,
        None,
        PageBudget::default(),
    )
    .await
    .unwrap();
    assert_eq!(ids(&descending.rows), [6, 5]);
    let next = page(
        &mut store,
        scope,
        &spec(super::super::Direction::Desc),
        2,
        descending.cursor.as_ref(),
        PageBudget::default(),
    )
    .await
    .unwrap();
    assert_eq!(ids(&next.rows), [4, 1]);
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; stable live-range proof"]
async fn growth_overflow_is_explicit_and_larger_budget_preserves_boundaries() {
    let (mut store, scope) = fixture().await;
    for id in [10, 20, 30, 40] {
        insert(&store, scope, id, id, "x").await;
    }
    let budget = PageBudget {
        max_rows: 3,
        ..PageBudget::default()
    };
    let first = page(
        &mut store,
        scope,
        &spec(super::super::Direction::Asc),
        2,
        None,
        budget,
    )
    .await
    .unwrap();
    let second = page(
        &mut store,
        scope,
        &spec(super::super::Direction::Asc),
        2,
        first.cursor.as_ref(),
        budget,
    )
    .await
    .unwrap();
    let ranges = [first.range.clone(), second.range.clone()];
    // New rows grow the first fixed interval; the second interval stays valid.
    for id in [11, 12] {
        insert(&store, scope, id, id, "x").await;
    }
    // Evaluate the still-valid sibling first, then reject the oversized range.
    let result = refresh(
        &mut store,
        scope,
        &[ranges[1].clone(), ranges[0].clone()],
        budget,
    )
    .await;
    assert!(matches!(result, Err(Error::QuotaExceeded)));
    // Raising the trusted engine budget recovers all rows, with no cursor reset.
    let published = refresh(&mut store, scope, &ranges, PageBudget::default())
        .await
        .unwrap();
    assert_eq!(
        published
            .iter()
            .flat_map(|page| ids(&page.rows))
            .collect::<Vec<_>>(),
        [10, 11, 12, 20, 30, 40]
    );
    assert_eq!(published[0].range, ranges[0]);
    println!(
        "range_growth initial_rows=2 refreshed_rows={} physical_row_json_bytes={}",
        published[0].rows.len(),
        published[0].row_json_bytes
    );
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; stable live-range proof"]
async fn cursor_binding_empty_ranges_and_byte_budgets_do_not_fake_exhaustion() {
    let (mut store, scope) = fixture().await;
    let empty = page(
        &mut store,
        scope,
        &spec(super::super::Direction::Asc),
        1,
        None,
        PageBudget::default(),
    )
    .await
    .unwrap();
    assert!(empty.rows.is_empty());
    assert!(empty.cursor.is_none());
    insert(&store, scope, 1, 1, "x").await;
    insert(&store, scope, 2, 2, "x").await;
    let grown = refresh(
        &mut store,
        scope,
        std::slice::from_ref(&empty.range),
        PageBudget::default(),
    )
    .await
    .unwrap();
    assert_eq!(ids(&grown[0].rows), [1, 2]);
    let first = page(
        &mut store,
        scope,
        &spec(super::super::Direction::Asc),
        1,
        None,
        PageBudget {
            max_rows: 1,
            ..PageBudget::default()
        },
    )
    .await
    .unwrap();
    let cursor = first.cursor.as_ref().unwrap();
    assert!(matches!(
        page(
            &mut store,
            scope,
            &spec(super::super::Direction::Desc),
            1,
            Some(cursor),
            PageBudget::default()
        )
        .await,
        Err(Error::InvalidInput(_))
    ));
    let mut other_filter = spec(super::super::Direction::Asc);
    other_filter.filters = vec![super::super::Predicate::Constant { value: false }];
    assert!(matches!(
        page(
            &mut store,
            scope,
            &other_filter,
            1,
            Some(cursor),
            PageBudget::default()
        )
        .await,
        Err(Error::InvalidInput(_))
    ));
    let other_scope = Uuid::now_v7();
    store.create_scope(other_scope).await.unwrap();
    assert!(matches!(
        page(
            &mut store,
            other_scope,
            &spec(super::super::Direction::Asc),
            1,
            Some(cursor),
            PageBudget::default()
        )
        .await,
        Err(Error::InvalidInput(_))
    ));
    insert(&store, scope, 3, 3, &"large".repeat(10_000)).await;
    // A large lookahead row must not consume the first page's byte quota.
    let bounded = page(
        &mut store,
        scope,
        &spec(super::super::Direction::Asc),
        2,
        None,
        PageBudget {
            max_row_json_bytes: 1024,
            ..PageBudget::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(ids(&bounded.rows), [1, 2]);
    assert!(bounded.cursor.is_some());

    assert!(matches!(
        refresh(
            &mut store,
            scope,
            std::slice::from_ref(&empty.range),
            PageBudget {
                max_row_json_bytes: 256,
                ..PageBudget::default()
            }
        )
        .await,
        Err(Error::QuotaExceeded)
    ));
    // An excluded large value must not be fetched or charged to the page.
    // Cursor fields remain private, but are still fetched and budgeted.
    let mut projected = spec(super::super::Direction::Asc);
    projected.projection = Some(vec!["group".into()]);
    let budget = PageBudget {
        max_row_json_bytes: 256,
        ..PageBudget::default()
    };
    let first = page(&mut store, scope, &projected, 2, None, budget)
        .await
        .unwrap();
    assert_eq!(first.rows.len(), 2);
    assert!(first.rows.iter().all(|row| row.values
        == hulladb_schema::RecordValues::from([("group".into(), Value::String("a".into()))])));
    let next = page(
        &mut store,
        scope,
        &projected,
        2,
        first.cursor.as_ref(),
        budget,
    )
    .await
    .unwrap();
    assert_eq!(next.rows.len(), 1);
    assert_eq!(next.rows[0].values.len(), 1);
    assert!(next.cursor.is_none());
    let refreshed = refresh(
        &mut store,
        scope,
        &[first.range.clone(), next.range],
        budget,
    )
    .await
    .unwrap();
    assert_eq!(
        refreshed
            .iter()
            .map(|page| page.rows.len())
            .collect::<Vec<_>>(),
        [2, 1]
    );
    assert!(
        refreshed
            .iter()
            .flat_map(|page| &page.rows)
            .all(|row| row.values.len() == 1)
    );
    let mut changed = projected.clone();
    changed.projection = Some(vec!["id".into()]);
    assert!(matches!(
        page(
            &mut store,
            scope,
            &changed,
            2,
            first.cursor.as_ref(),
            budget
        )
        .await,
        Err(Error::InvalidInput(_))
    ));
    for selection in [
        vec![],
        vec!["absent".into()],
        vec!["id".into(), "id".into()],
    ] {
        changed.projection = Some(selection);
        assert!(matches!(
            page(&mut store, scope, &changed, 2, None, budget).await,
            Err(Error::InvalidInput(_))
        ));
    }
    projected.order = vec![super::super::Order {
        field: "text".into(),
        direction: super::super::Direction::Asc,
    }];
    assert!(matches!(
        page(&mut store, scope, &projected, 3, None, budget).await,
        Err(Error::QuotaExceeded)
    ));
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; nullable mixed ordering and untrusted tokens"]
async fn mixed_orders_cross_null_boundaries_and_tokens_preserve_query_identity() {
    use super::super::{Direction, Order, Predicate};
    let (mut store, scope) = fixture().await;
    for id in 1..=10 {
        insert(&store, scope, id, (id + 1) / 2, "x").await;
    }
    store
        .client
        .execute(
            &format!(
                "UPDATE {} SET f1=NULL WHERE _scope=$1 AND f0>=7",
                store.schema.relation("t0")
            ),
            &[&scope],
        )
        .await
        .unwrap();
    store
        .client
        .execute(
            &format!(
                "UPDATE {} SET f2='b' WHERE _scope=$1 AND f0 IN (2,4,6,8,10)",
                store.schema.relation("t0")
            ),
            &[&scope],
        )
        .await
        .unwrap();
    for (rank, group, expected) in [
        (
            Direction::Asc,
            Direction::Desc,
            vec![2, 1, 4, 3, 6, 5, 10, 8, 9, 7],
        ),
        (
            Direction::Desc,
            Direction::Asc,
            vec![7, 9, 8, 10, 5, 6, 3, 4, 1, 2],
        ),
    ] {
        let query = PageQuery {
            table: "items".into(),
            projection: None,
            filters: vec![Predicate::Constant { value: true }],
            order: vec![
                Order {
                    field: "rank".into(),
                    direction: rank,
                },
                Order {
                    field: "group".into(),
                    direction: group,
                },
            ],
        };
        let mut cursor = None;
        let mut collected = Vec::new();
        let mut ranges = Vec::new();
        loop {
            let loaded = page(
                &mut store,
                scope,
                &query,
                2,
                cursor.as_ref(),
                PageBudget::default(),
            )
            .await
            .unwrap();
            collected.extend(ids(&loaded.rows));
            let token = loaded.range.encode().unwrap();
            let tx = store.client.transaction().await.unwrap();
            let reads = Mutex::new(Default::default());
            let read_budget = Mutex::new(super::super::db::ReadBudget::default());
            let db = ReadDb {
                tx: &tx,
                schema: &store.schema,
                scope,
                reads: &reads,
                read_budget: &read_budget,
            };
            let range = db
                .decode_page_range(&query, &token, cursor.as_ref())
                .await
                .unwrap();
            assert_eq!(range, loaded.range);
            if let Some(prior) = ranges.first() {
                let prior_token = PageRange::encode(prior).unwrap();
                assert!(matches!(
                    db.decode_page_range(&query, &prior_token, cursor.as_ref())
                        .await,
                    Err(Error::InvalidInput(_))
                ));
            }
            tx.rollback().await.unwrap();
            ranges.push(range);
            cursor = loaded
                .cursor
                .as_ref()
                .map(|cursor| PageCursor::decode(&cursor.encode().unwrap()).unwrap());
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(collected, expected);
        let refreshed = refresh(&mut store, scope, &ranges, PageBudget::default())
            .await
            .unwrap();
        assert_eq!(
            refreshed
                .iter()
                .flat_map(|page| ids(&page.rows))
                .collect::<Vec<_>>(),
            expected
        );
        let mut wrong = query.clone();
        wrong.filters = vec![Predicate::Constant { value: false }];
        let first = page(&mut store, scope, &query, 1, None, PageBudget::default())
            .await
            .unwrap();
        assert!(matches!(
            page(
                &mut store,
                scope,
                &wrong,
                1,
                first.cursor.as_ref(),
                PageBudget::default()
            )
            .await,
            Err(Error::InvalidInput(_))
        ));
    }
    cleanup(&store).await;
}
