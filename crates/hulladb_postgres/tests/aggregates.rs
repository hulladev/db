use hulladb_postgres::managed::{
    Aggregate, AggregateOp, AggregateQuery, Direction, Error, ManagedSchema, ManagedStore, Operand,
    Order, Predicate,
};
use hulladb_schema::{
    FieldDefinition, RecordValues, ScalarType, SchemaDeclaration, TableDeclaration, Value,
};
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;
use uuid::Uuid;

fn compare(field: &str, operator: &str, value: Json) -> Predicate {
    Predicate::Compare {
        field: field.into(),
        transform: None,
        operator: operator.into(),
        operand: Operand::Value { value },
        case_sensitive: None,
    }
}
fn summary() -> AggregateQuery {
    AggregateQuery {
        table: "items".into(),
        filters: vec![],
        group_by: vec![],
        having: vec![],
        order: vec![],
        limit: None,
        aggregates: BTreeMap::from([
            (
                "count".into(),
                Aggregate {
                    op: AggregateOp::Count,
                    field: None,
                },
            ),
            (
                "total".into(),
                Aggregate {
                    op: AggregateOp::Sum,
                    field: Some("amount".into()),
                },
            ),
            (
                "average".into(),
                Aggregate {
                    op: AggregateOp::Avg,
                    field: Some("amount".into()),
                },
            ),
            (
                "score".into(),
                Aggregate {
                    op: AggregateOp::Sum,
                    field: Some("score".into()),
                },
            ),
            (
                "mean".into(),
                Aggregate {
                    op: AggregateOp::Avg,
                    field: Some("score".into()),
                },
            ),
            (
                "lowest".into(),
                Aggregate {
                    op: AggregateOp::Min,
                    field: Some("amount".into()),
                },
            ),
            (
                "highest".into(),
                Aggregate {
                    op: AggregateOp::Max,
                    field: Some("label".into()),
                },
            ),
        ]),
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; aggregate precision, SQL nulls, grouped filters and scoped snapshots"]
async fn aggregates_preserve_precision_scope_and_group_semantics() {
    let url = std::env::var("HULLADB_TEST_URL")
        .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into());
    let mut amount = FieldDefinition::new("amount", ScalarType::Int64);
    amount.nullable = true;
    let mut score = FieldDefinition::new("score", ScalarType::Float64);
    score.nullable = true;
    let mut label = FieldDefinition::new("label", ScalarType::String);
    label.nullable = true;
    let contract = SchemaDeclaration::new(vec![TableDeclaration::new(
        "items",
        vec![
            FieldDefinition::new("id", ScalarType::Int64),
            label,
            amount,
            score,
        ],
    )])
    .normalize()
    .unwrap();
    let schema =
        ManagedSchema::new(format!("aggregate_{}", Uuid::now_v7().simple()), contract).unwrap();
    let mut store = ManagedStore::connect(&url, schema.clone()).await.unwrap();
    store.initialize().await.unwrap();
    let scope = Uuid::now_v7();
    let other_scope = Uuid::now_v7();
    store.create_scope(scope).await.unwrap();
    store.create_scope(other_scope).await.unwrap();
    for target in [scope, other_scope] {
        store
            .direct_transaction(target, true, |_, write| {
                Box::pin(async move {
                    let db = write.unwrap();
                    for (id, label, amount, score) in [
                        (1, Some("a"), Some(i64::MAX), Some(1.25)),
                        (2, Some("a"), Some(i64::MAX), Some(2.75)),
                        (3, Some("b"), Some(9), Some(10.0)),
                        (4, Some("b"), Some(10), None),
                        (5, Some("c"), None, None),
                        (6, None, Some(2), Some(-1.0)),
                        (7, Some("d"), Some(2), Some(0.0)),
                    ] {
                        db.insert(
                            "items",
                            RecordValues::from([
                                ("id".into(), Value::Int64(id)),
                                (
                                    "label".into(),
                                    label
                                        .map(|v| Value::String(v.into()))
                                        .unwrap_or(Value::Null),
                                ),
                                (
                                    "amount".into(),
                                    amount.map(Value::Int64).unwrap_or(Value::Null),
                                ),
                                (
                                    "score".into(),
                                    score.and_then(Value::float64).unwrap_or(Value::Null),
                                ),
                            ]),
                        )
                        .await?;
                    }
                    Ok(((), true))
                })
            })
            .await
            .unwrap();
    }
    store.direct_transaction(scope, false, |read, _| Box::pin(async move {
        let result = read.aggregate(&summary()).await?;
        assert_eq!(result["count"], 7);
        assert_eq!(result["total"], "18446744073709551637");
        assert_eq!(result["score"], 13.0);
        assert_eq!(result["mean"], 2.6);
        assert_eq!(result["lowest"], "2");
        assert_eq!(result["highest"], "d");
        let mut query = summary(); query.filters = vec![compare("id", "<", json!("0"))];
        assert_eq!(read.aggregate(&query).await?, json!({"count":0,"total":null,"average":null,"score":null,"mean":null,"lowest":null,"highest":null}));
        query.group_by = vec!["label".into()];
        assert_eq!(read.aggregate(&query).await?, json!([]));
        query.filters.clear();
        query.order = vec![Order { field: "total".into(), direction: Direction::Asc }];
        let groups = read.aggregate(&query).await?;
        assert_eq!(groups.as_array().unwrap().iter().map(|v| v["label"].clone()).collect::<Vec<_>>(), vec![json!("d"), Json::Null, json!("b"), json!("a"), json!("c")]);
        assert_eq!(groups[2]["average"], "9.5000000000000000");
        assert_eq!(groups[4]["total"], Json::Null);
        query.having = vec![compare("total", ">", json!("9223372036854775807"))];
        assert_eq!(read.aggregate(&query).await?.as_array().unwrap().len(), 1);
        query.having = vec![compare("average", "between", json!(["9.4", "9.6"]))];
        assert_eq!(read.aggregate(&query).await?[0]["label"], "b");
        query.having = vec![Predicate::Compare {
            field: "total".into(), transform: None, operator: ">".into(),
            operand: Operand::Field { field: "lowest".into(), transform: None }, case_sensitive: None,
        }];
        assert_eq!(read.aggregate(&query).await?.as_array().unwrap().iter().map(|v| v["label"].clone()).collect::<Vec<_>>(), vec![json!("b"), json!("a")]);
        query.having = vec![Predicate::Not { predicate: Box::new(compare("average", "<", json!("3"))) }];
        query.order = vec![Order { field: "total".into(), direction: Direction::Desc }]; query.limit = Some(1);
        assert_eq!(read.aggregate(&query).await?[0]["label"], "a");
        // Filtering rows before grouping changes the computed aggregate, while
        // HAVING subsequently restricts the resulting groups.
        query.filters = vec![compare("id", "!=", json!("1"))];
        query.having = vec![compare("count", "=", json!(1)), compare("total", "!=", Json::Null)];
        assert_eq!(read.aggregate(&query).await?[0]["total"], i64::MAX.to_string());
        for bad in [
            { let mut q = summary(); q.aggregates.get_mut("total").unwrap().field = Some("missing".into()); q },
            { let mut q = summary(); q.group_by = vec!["label".into()]; q.aggregates.insert("label".into(), Aggregate { op: AggregateOp::Count, field: None }); q },
            { let mut q = summary(); q.aggregates.get_mut("count").unwrap().field = Some("id".into()); q },
            { let mut q = summary(); q.group_by = vec!["label".into()]; q.having = vec![compare("average", "contains", json!("9"))]; q },
            { let mut q = summary(); q.group_by = vec!["label".into()]; q.having = vec![compare("total", ">", json!("01"))]; q },
            { let mut q = summary(); q.order = vec![Order { field: "count".into(), direction: Direction::Asc }]; q },
        ] { assert!(matches!(read.aggregate(&bad).await, Err(Error::InvalidInput(_)))); }
        Ok(((), true))
    })).await.unwrap();
    let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
        .await
        .unwrap();
    let task = tokio::spawn(connection);
    client
        .batch_execute(&format!("DROP SCHEMA \"{}\" CASCADE", schema.namespace()))
        .await
        .unwrap();
    task.abort();
}
