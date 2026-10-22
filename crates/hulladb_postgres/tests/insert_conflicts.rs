use std::collections::BTreeMap;

use hulladb_postgres::managed::{
    Error, InsertConflict, InsertConflictAction, ManagedSchema, ManagedStore, Select,
    WriteExpression, WriteSource,
};
use hulladb_schema::{
    FieldDefinition, RecordValues, ScalarType, SchemaDeclaration, TableDeclaration,
    UniqueConstraintDeclaration, Value,
};
use serde_json::json;
use uuid::Uuid;

fn row(id: i64, email: &str, team: &str, name: &str, count: i64) -> RecordValues {
    RecordValues::from([
        ("id".into(), Value::Int64(id)),
        ("email".into(), Value::String(email.into())),
        ("team".into(), Value::String(team.into())),
        ("name".into(), Value::String(name.into())),
        ("count".into(), Value::Int64(count)),
        ("optional".into(), Value::Null),
    ])
}
fn policy(fields: &[&str], action: InsertConflictAction) -> InsertConflict {
    InsertConflict {
        fields: fields.iter().map(|v| (*v).into()).collect(),
        action,
    }
}
fn field(source: WriteSource, name: &str) -> WriteExpression {
    WriteExpression::Field {
        source,
        field: name.into(),
    }
}
fn increment() -> InsertConflictAction {
    InsertConflictAction::Update {
        values: BTreeMap::from([
            (
                "count".into(),
                WriteExpression::Add {
                    left: Box::new(field(WriteSource::Current, "count")),
                    right: Box::new(field(WriteSource::Incoming, "count")),
                },
            ),
            ("name".into(), field(WriteSource::Incoming, "name")),
        ]),
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; atomic conflict statements, constraint enforcement and scopes"]
async fn conflicts_are_atomic_scoped_and_return_only_written_rows() {
    let url = std::env::var("HULLADB_TEST_URL")
        .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into());
    let mut optional = FieldDefinition::new("optional", ScalarType::Int64);
    optional.nullable = true;
    let mut table = TableDeclaration::new(
        "users",
        vec![
            FieldDefinition::new("id", ScalarType::Int64),
            FieldDefinition::new("email", ScalarType::String),
            FieldDefinition::new("team", ScalarType::String),
            FieldDefinition::new("name", ScalarType::String),
            FieldDefinition::new("count", ScalarType::Int64),
            optional,
        ],
    );
    table.unique_constraints = vec![
        UniqueConstraintDeclaration::new("email", vec!["email".into()]),
        UniqueConstraintDeclaration::new("team_name", vec!["team".into(), "name".into()]),
    ];
    let schema = ManagedSchema::new(
        format!("conflict_{}", Uuid::now_v7().simple()),
        SchemaDeclaration::new(vec![table]).normalize().unwrap(),
    )
    .unwrap();
    let mut store = ManagedStore::connect(&url, schema.clone()).await.unwrap();
    store.initialize().await.unwrap();
    let scope = Uuid::now_v7();
    let other = Uuid::now_v7();
    store.create_scope(scope).await.unwrap();
    store.create_scope(other).await.unwrap();
    for target in [scope, other] {
        store
            .direct_transaction(target, true, |_, write| {
                Box::pin(async move {
                    write
                        .unwrap()
                        .insert("users", row(1, "a", "one", "A", 2))
                        .await?;
                    Ok(((), true))
                })
            })
            .await
            .unwrap();
    }
    store
        .direct_transaction(scope, true, |_, write| {
            Box::pin(async move {
                let db = write.unwrap();
                let projection = vec!["count".into(), "name".into()];
                let updated = db
                    .insert_many_projected(
                        "users",
                        vec![row(2, "a", "one", "B", 3)],
                        Some(&projection),
                        Some(&policy(&["email"], increment())),
                    )
                    .await?;
                assert_eq!(updated.len(), 1);
                assert_eq!(
                    updated[0].values,
                    RecordValues::from([
                        ("count".into(), Value::Int64(5)),
                        ("name".into(), Value::String("B".into()))
                    ])
                );
                assert_eq!(updated[0].version, 2);
                // Only the inserted second row returns; the primary-key skip does not
                // pretend to have returned the unchanged first row.
                let written = db
                    .insert_many_projected(
                        "users",
                        vec![row(1, "a", "one", "B", 99), row(3, "c", "two", "C", 4)],
                        None,
                        Some(&policy(&["id"], InsertConflictAction::Skip)),
                    )
                    .await?;
                assert_eq!(written.len(), 1);
                assert_eq!(written[0].values["id"], Value::Int64(3));
                let skipped = db
                    .insert_many_projected(
                        "users",
                        vec![row(4, "d", "two", "C", 1)],
                        None,
                        Some(&policy(&["name", "team"], InsertConflictAction::Skip)),
                    )
                    .await?;
                assert!(skipped.is_empty());
                let expressions = BTreeMap::from([(
                    "count".into(),
                    WriteExpression::Add {
                        left: Box::new(field(WriteSource::Current, "count")),
                        right: Box::new(WriteExpression::Value { value: json!("2") }),
                    },
                )]);
                let changed = db
                    .update_expressions_projected(
                        "users",
                        &[hulladb_postgres::managed::Predicate::Constant { value: true }],
                        expressions,
                        None,
                    )
                    .await?;
                assert_eq!(changed.len(), 2);
                let nullable = BTreeMap::from([(
                    "optional".into(),
                    WriteExpression::Add {
                        left: Box::new(field(WriteSource::Current, "optional")),
                        right: Box::new(WriteExpression::Value { value: json!("2") }),
                    },
                )]);
                let unchanged = db
                    .update_expressions_projected(
                        "users",
                        &[hulladb_postgres::managed::Predicate::Constant { value: true }],
                        nullable,
                        None,
                    )
                    .await?;
                assert!(
                    unchanged
                        .iter()
                        .all(|row| row.values["optional"] == Value::Null)
                );
                let mut oversized = field(WriteSource::Current, "count");
                for _ in 0..10 {
                    oversized = WriteExpression::Add {
                        left: Box::new(oversized.clone()),
                        right: Box::new(oversized),
                    };
                }
                assert!(matches!(
                    db.update_expressions_projected(
                        "users",
                        &[hulladb_postgres::managed::Predicate::Constant { value: true }],
                        BTreeMap::from([("count".into(), oversized)]),
                        None
                    )
                    .await,
                    Err(Error::InvalidInput(_))
                ));

                for bad in [
                    policy(&["name"], InsertConflictAction::Skip),
                    policy(&["id", "id"], InsertConflictAction::Skip),
                ] {
                    assert!(matches!(
                        db.insert_many_projected("users", vec![], None, Some(&bad))
                            .await,
                        Err(Error::InvalidInput(_))
                    ));
                }
                Ok(((), true))
            })
        })
        .await
        .unwrap();
    // Resolving one unique key cannot ignore a different uniqueness rule.
    let rejected = store
        .direct_transaction(scope, true, |_, write| {
            Box::pin(async move {
                let action = InsertConflictAction::Update {
                    values: BTreeMap::from([(
                        "email".into(),
                        WriteExpression::Value { value: json!("c") },
                    )]),
                };
                write
                    .unwrap()
                    .insert_many_projected(
                        "users",
                        vec![row(1, "new", "three", "D", 8)],
                        None,
                        Some(&policy(&["id"], action)),
                    )
                    .await?;
                Ok(((), true))
            })
        })
        .await;
    assert!(matches!(rejected, Err(Error::Conflict)));
    // PostgreSQL rejects a single statement that updates the same conflicting
    // row twice, rather than leaving an order-dependent accumulated update.
    assert!(
        store
            .direct_transaction(scope, true, |_, write| Box::pin(async move {
                write
                    .unwrap()
                    .insert_many_projected(
                        "users",
                        vec![row(4, "a", "one", "E", 1), row(5, "a", "one", "F", 1)],
                        None,
                        Some(&policy(&["email"], increment())),
                    )
                    .await?;
                Ok(((), true))
            }))
            .await
            .is_err()
    );
    // Arithmetic remains checked by PostgreSQL; it must not wrap the stored count.
    assert!(
        store
            .direct_transaction(scope, true, |_, write| Box::pin(async move {
                write
                    .unwrap()
                    .insert_many_projected(
                        "users",
                        vec![row(8, "a", "one", "overflow", i64::MAX)],
                        None,
                        Some(&policy(&["email"], increment())),
                    )
                    .await?;
                Ok(((), true))
            }))
            .await
            .is_err()
    );
    // A later failure rolls back the earlier conflict update in the same transaction.
    assert!(
        store
            .direct_transaction(scope, true, |_, write| Box::pin(async move {
                let db = write.unwrap();
                db.insert_many_projected(
                    "users",
                    vec![row(4, "a", "one", "G", 100)],
                    None,
                    Some(&policy(&["email"], increment())),
                )
                .await?;
                db.insert("users", row(3, "duplicate", "four", "H", 1))
                    .await?;
                Ok(((), true))
            }))
            .await
            .is_err()
    );
    for (target, expected) in [(scope, 7), (other, 2)] {
        store
            .direct_transaction(target, false, |read, _| {
                Box::pin(async move {
                    let users = read.select("users", Select::default()).await?;
                    assert_eq!(users[0].values["count"], Value::Int64(expected));
                    assert_eq!(
                        users[0].values["name"],
                        Value::String(if target == scope { "B" } else { "A" }.into())
                    );
                    Ok(((), true))
                })
            })
            .await
            .unwrap();
    }
    let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
        .await
        .unwrap();
    let task = tokio::spawn(async move { connection.await.unwrap() });
    client
        .batch_execute(&format!("DROP SCHEMA {} CASCADE", schema.namespace()))
        .await
        .unwrap();
    drop(client);
    task.await.unwrap();
}
