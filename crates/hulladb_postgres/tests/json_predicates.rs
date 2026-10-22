use hulladb_postgres::managed::{ManagedSchema, ManagedStore, Operand, Predicate};
use hulladb_schema::{
    FieldDefinition, JsonProperty, JsonShape, RecordValues, ScalarType, SchemaDeclaration,
    TableDeclaration, Value,
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
#[tokio::test]
#[ignore = "requires PostgreSQL; native JSON storage and predicate semantics"]
async fn json_paths_exact_array_membership_nulls_and_replacement() {
    let url = std::env::var("HULLADB_TEST_URL")
        .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into());
    let mut settings = FieldDefinition::new("settings", ScalarType::Json);
    settings.nullable = true;
    settings.json_shape = Some(JsonShape::Object {
        fields: BTreeMap::from([
            (
                "tags".into(),
                JsonProperty {
                    optional: true,
                    shape: JsonShape::Nullable {
                        inner: Box::new(JsonShape::Array {
                            item: Box::new(JsonShape::Any),
                        }),
                    },
                },
            ),
            (
                "theme".into(),
                JsonProperty {
                    optional: true,
                    shape: JsonShape::Nullable {
                        inner: Box::new(JsonShape::String),
                    },
                },
            ),
            (
                "literal.key".into(),
                JsonProperty {
                    optional: true,
                    shape: JsonShape::String,
                },
            ),
        ]),
    });
    let contract = SchemaDeclaration::new(vec![TableDeclaration::new(
        "items",
        vec![FieldDefinition::new("id", ScalarType::Int64), settings],
    )])
    .normalize()
    .unwrap();
    let schema = ManagedSchema::new(format!("json_{}", Uuid::now_v7().simple()), contract).unwrap();
    let mut store = ManagedStore::connect(&url, schema.clone()).await.unwrap();
    store.initialize().await.unwrap();
    let scope = Uuid::now_v7();
    store.create_scope(scope).await.unwrap();
    store.direct_transaction(scope, true, |_, write| Box::pin(async move {
        let db = write.unwrap();
        for (id, settings) in [
            (1, Value::Json(json!({"tags":[{"a":1,"b":2},[1,2],null],"theme":null,"literal.key":"yes"}))),
            (2, Value::Json(json!({}))),
            (3, Value::Null),
            (4, Value::Json(json!({"tags":[]}))),
            (5, Value::Json(json!({"tags":[null]}))),
            (6, Value::Json(json!({"tags":null,"theme":"dark"}))),
        ] { db.insert("items", RecordValues::from([("id".into(),Value::Int64(id)),("settings".into(),settings)])).await?; }
        Ok(((),true))
    })).await.unwrap();
    store
        .direct_transaction(scope, false, |read, _| {
            Box::pin(async move {
                for (predicate, expected) in [
                    (compare("settings.tags", "contains", json!({"a":1})), vec![]),
                    (
                        compare("settings.tags", "contains", json!({"b":2,"a":1})),
                        vec![1],
                    ),
                    (compare("settings.tags", "contains", json!([1, 2])), vec![1]),
                    (compare("settings.tags", "contains", Json::Null), vec![1, 5]),
                    (compare("settings.tags", "containsAny", json!([])), vec![]),
                    (
                        compare("settings.tags", "containsAll", json!([])),
                        vec![1, 4, 5],
                    ),
                    (compare("settings.tags", "containedBy", json!([])), vec![4]),
                    (
                        Predicate::Not {
                            predicate: Box::new(compare("settings.tags", "containsAny", json!([]))),
                        },
                        vec![1, 4, 5],
                    ),
                    (compare("settings.theme", "exists", json!(true)), vec![1, 6]),
                    (
                        compare("settings.theme", "=", Json::Null),
                        vec![1, 2, 3, 4, 5],
                    ),
                    (compare("settings.theme", "!=", Json::Null), vec![6]),
                    (compare("settings.theme", "contains", json!("ar")), vec![6]),
                    (compare("settings.tags", "=", json!([])), vec![4]),
                ] {
                    let rows = read
                        .select_where("items", std::slice::from_ref(&predicate), None, &[])
                        .await?;
                    let ids = rows
                        .iter()
                        .map(|r| match r.values["id"] {
                            Value::Int64(n) => n,
                            _ => panic!(),
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(ids, expected, "{predicate:?}");
                }
                let mut length = compare("settings.tags", ">=", json!(3));
                if let Predicate::Compare { transform, .. } = &mut length {
                    *transform = Some("arrayLength".into());
                }
                assert_eq!(
                    read.select_where("items", &[length], None, &[])
                        .await?
                        .len(),
                    1
                );
                let mut literal = compare("ignored", "=", json!("yes"));
                if let Predicate::Compare { field, .. } = &mut literal {
                    *field = vec!["settings".into(), "literal.key".into()].into();
                }
                assert_eq!(
                    read.select_where("items", &[literal], None, &[])
                        .await?
                        .len(),
                    1
                );
                Ok(((), true))
            })
        })
        .await
        .unwrap();
    store
        .direct_transaction(scope, true, |_, write| {
            Box::pin(async move {
                let row = write
                    .unwrap()
                    .update(
                        "items",
                        Value::Int64(1),
                        1,
                        RecordValues::from([("settings".into(), Value::Json(json!({"tags":[]})))]),
                    )
                    .await?;
                assert_eq!(row.values["settings"], Value::Json(json!({"tags":[]})));
                Ok(((), true))
            })
        })
        .await
        .unwrap();
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
