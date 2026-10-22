use hulladb_postgres::managed::{Error, ManagedSchema, ManagedStore, Predicate};
use hulladb_schema::{
    FieldDefinition, RecordValues, ScalarType, SchemaDeclaration, TableDeclaration, Value,
};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires PostgreSQL; cumulative fetched rows/bytes and poisoned write rollback"]
async fn fetched_budgets_accumulate_and_caught_returning_errors_cannot_commit() {
    let url = std::env::var("HULLADB_TEST_URL")
        .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into());
    let contract = SchemaDeclaration::new(vec![TableDeclaration::new(
        "items",
        vec![
            FieldDefinition::new("id", ScalarType::Int64),
            FieldDefinition::new("body", ScalarType::String),
        ],
    )])
    .normalize()
    .unwrap();
    let schema =
        ManagedSchema::new(format!("read_budget_{}", Uuid::now_v7().simple()), contract).unwrap();
    let mut store = ManagedStore::connect(&url, schema.clone()).await.unwrap();
    store.initialize().await.unwrap();
    let scope = Uuid::now_v7();
    store.create_scope(scope).await.unwrap();
    let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
        .await
        .unwrap();
    let task = tokio::spawn(connection);
    client.execute(&format!("INSERT INTO \"{}\".t0 (_scope,_version,f0,f1) SELECT $1,1,i,repeat('x',2048) FROM generate_series(1,100) i", schema.namespace()), &[&scope]).await.unwrap();
    let error = store
        .direct_transaction(scope, false, |read, _| {
            Box::pin(async move {
                read.set_read_limits(2, 10_000)?;
                let projection = Some(vec!["id".into()]);
                for _ in 0..2 {
                    read.select_where_projected("items", &[], Some(1), &[], projection.as_deref())
                        .await?;
                }
                // Both projected int64 and private concurrency version occupy 8 bytes.
                assert_eq!(read.read_usage()?, (2, 32));
                read.select_where_projected("items", &[], Some(1), &[], projection.as_deref())
                    .await?;
                Ok(((), true))
            })
        })
        .await
        .unwrap_err();
    assert!(matches!(error, Error::TransactionDeadline));
    let error = store
        .direct_transaction(scope, false, |read, _| {
            Box::pin(async move {
                read.set_read_limits(1000, 1024)?;
                read.select_where_projected("items", &[], None, &[], Some(&["body".into()]))
                    .await?;
                Ok(((), true))
            })
        })
        .await
        .unwrap_err();
    assert!(matches!(error, Error::TransactionDeadline));
    let error = store
        .direct_transaction(scope, true, |read, write| {
            Box::pin(async move {
                read.set_read_limits(2, 100_000)?;
                let result = write
                    .unwrap()
                    .update_where(
                        "items",
                        &[Predicate::Constant { value: true }],
                        RecordValues::from([("body".into(), Value::String("changed".into()))]),
                    )
                    .await;
                assert!(result.is_err());
                // Swallowing the operation failure still cannot commit its SQL writes.
                Ok(((), true))
            })
        })
        .await
        .unwrap_err();
    assert!(matches!(error, Error::TransactionDeadline));
    let changed: i64 = client
        .query_one(
            &format!(
                "SELECT count(*) FROM \"{}\".t0 WHERE f1='changed'",
                schema.namespace()
            ),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(changed, 0);
    client
        .batch_execute(&format!("DROP SCHEMA \"{}\" CASCADE", schema.namespace()))
        .await
        .unwrap();
    task.abort();
}
