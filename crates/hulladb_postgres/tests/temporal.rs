use hulladb_postgres::managed::{
    Aggregate, AggregateOp, AggregateQuery, Direction, ManagedSchema, ManagedStore, Operand, Order,
    PageBudget, PageQuery, Predicate, WriteExpression, WriteSource, WriteValues,
};
use hulladb_schema::{
    FieldDefinition, RecordValues, ScalarType, SchemaDeclaration, TableDeclaration, Value,
};
use serde_json::json;
use std::collections::BTreeMap;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires PostgreSQL; native calendar storage, precision, predicates and cursor boundaries"]
async fn calendar_storage_preserves_precision_and_chronological_queries() {
    let url = std::env::var("HULLADB_TEST_URL")
        .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into());
    let mut fine = FieldDefinition::new("fine", ScalarType::Timestamp);
    fine.precision = Some(6);
    let contract = SchemaDeclaration::new(vec![TableDeclaration::new(
        "events",
        vec![
            FieldDefinition::new("id", ScalarType::Int64),
            FieldDefinition::new("day", ScalarType::Date),
            FieldDefinition::new("instant", ScalarType::Timestamp),
            FieldDefinition::new("clock", ScalarType::LocalTimestamp),
            fine,
        ],
    )])
    .normalize()
    .unwrap();
    let schema =
        ManagedSchema::new(format!("temporal_{}", Uuid::now_v7().simple()), contract).unwrap();
    let mut store = ManagedStore::connect(&url, schema.clone()).await.unwrap();
    store.initialize().await.unwrap();
    let scope = Uuid::now_v7();
    store.create_scope(scope).await.unwrap();
    store
        .direct_transaction(scope, true, |_, write| {
            Box::pin(async move {
                let db = write.unwrap();
                for id in [2, 1] {
                    let row = db
                        .insert(
                            "events",
                            RecordValues::from([
                                ("id".into(), Value::Int64(id)),
                                ("day".into(), Value::Date(format!("2024-02-2{}", 7 + id))),
                                (
                                    "instant".into(),
                                    Value::Timestamp(format!("2024-02-29T12:30:00.00{id}Z")),
                                ),
                                (
                                    "clock".into(),
                                    Value::LocalTimestamp("2024-10-27T02:30:00".into()),
                                ),
                                (
                                    "fine".into(),
                                    Value::Timestamp(format!("2024-02-29T12:30:00.00000{id}Z")),
                                ),
                            ]),
                        )
                        .await?;
                    assert_eq!(
                        row.values["fine"],
                        Value::Timestamp(format!("2024-02-29T12:30:00.00000{id}Z"))
                    );
                    assert_eq!(
                        row.values["clock"],
                        Value::LocalTimestamp("2024-10-27T02:30:00".into())
                    );
                }
                let bad = WriteValues::from([(
                    "instant".into(),
                    WriteExpression::Field {
                        source: WriteSource::Current,
                        field: "fine".into(),
                    },
                )]);
                assert!(
                    db.update_expressions_projected(
                        "events",
                        &[Predicate::Constant { value: true }],
                        bad,
                        None
                    )
                    .await
                    .is_err()
                );
                Ok(((), true))
            })
        })
        .await
        .unwrap();
    // A PostgreSQL offset literal must be returned as the same UTC instant, and
    // native DATE constraints also protect writes made outside the SDK.
    let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
        .await
        .unwrap();
    let task = tokio::spawn(connection);
    client.batch_execute(&format!("SET TIME ZONE 'America/New_York'; UPDATE \"{}\".t0 SET f2='2024-02-29T13:30:00.001+01:00'::timestamptz WHERE f0=1", schema.namespace())).await.unwrap();
    assert!(
        client
            .batch_execute(&format!(
                "UPDATE \"{}\".t0 SET f1='infinity'::date",
                schema.namespace()
            ))
            .await
            .is_err()
    );
    store
        .direct_transaction(scope, false, |read, _| {
            Box::pin(async move {
                let rows = read
                    .select_where_projected(
                        "events",
                        &[Predicate::Compare {
                            field: "instant".into(),
                            operator: "=".into(),
                            transform: None,
                            operand: Operand::Value {
                                value: json!("2024-02-29T12:30:00.001Z"),
                            },
                            case_sensitive: None,
                        }],
                        None,
                        &[],
                        Some(&["instant".into()]),
                    )
                    .await?;
                assert_eq!(rows.len(), 1);
                assert_eq!(
                    rows[0].values["instant"],
                    Value::Timestamp("2024-02-29T12:30:00.001Z".into())
                );
                let query = PageQuery {
                    table: "events".into(),
                    projection: Some(vec!["day".into()]),
                    filters: vec![],
                    order: vec![Order {
                        field: "fine".into(),
                        direction: Direction::Asc,
                    }],
                };
                let first = read.page(&query, 1, None, PageBudget::default()).await?;
                assert_eq!(
                    first.rows[0].values["day"],
                    Value::Date("2024-02-28".into())
                );
                let second = read
                    .page(&query, 1, first.cursor.as_ref(), PageBudget::default())
                    .await?;
                assert_eq!(
                    second.rows[0].values["day"],
                    Value::Date("2024-02-29".into())
                );
                let groups = read
                    .aggregate(&AggregateQuery {
                        table: "events".into(),
                        filters: vec![],
                        group_by: vec!["clock".into()],
                        aggregates: BTreeMap::from([(
                            "latest".into(),
                            Aggregate {
                                op: AggregateOp::Max,
                                field: Some("fine".into()),
                            },
                        )]),
                        having: vec![Predicate::Compare {
                            field: "latest".into(),
                            transform: None,
                            operator: ">".into(),
                            operand: Operand::Value {
                                value: json!("2024-02-29T12:30:00.000001Z"),
                            },
                            case_sensitive: None,
                        }],
                        order: vec![],
                        limit: None,
                    })
                    .await?;
                assert_eq!(
                    groups,
                    json!([{"clock":"2024-10-27T02:30:00", "latest":"2024-02-29T12:30:00.000002Z"}])
                );
                Ok(((), true))
            })
        })
        .await
        .unwrap();
    client
        .batch_execute(&format!("DROP SCHEMA \"{}\" CASCADE", schema.namespace()))
        .await
        .unwrap();
    task.abort();
}
