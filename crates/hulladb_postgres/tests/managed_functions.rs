use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use hulladb_postgres::managed::{
    Authorization, Context, Error, FunctionContract, FunctionFuture, Invocation, KeyGenerator,
    ManagedSchema, ManagedStore, Mutation, MutationContext, Query, QueryContext, Registry, Row,
    Select,
};
use hulladb_schema::{
    FieldDefinition, InsertKeyPolicy, KeyGeneration, RecordValues, ReferenceDeclaration,
    ScalarType, SchemaContract, SchemaDeclaration, TableDeclaration, UniqueConstraintDeclaration,
    UniqueNullPolicy, Value,
};
use serde_json::{Value as Json, json};
use uuid::Uuid;

const TITLE: &str = "title\"; DROP SCHEMA public; --";

fn schema() -> SchemaContract {
    let mut publications = TableDeclaration::new(
        "publications",
        vec![
            FieldDefinition::new("slug", ScalarType::String),
            FieldDefinition::new("name", ScalarType::String),
        ],
    );
    publications.primary_key = Some("slug".into());
    let mut published = FieldDefinition::new("published", ScalarType::Boolean);
    published.default = Some(Value::Boolean(false));
    let mut ordinal = FieldDefinition::new("ordinal", ScalarType::Int64);
    ordinal.default = Some(Value::Int64(i64::MAX));
    let mut excerpt = FieldDefinition::new("excerpt", ScalarType::String);
    excerpt.nullable = true;
    excerpt.default = Some(Value::Null);
    let mut articles = TableDeclaration::new(
        "articles",
        vec![
            FieldDefinition::new("publication", ScalarType::String),
            FieldDefinition::new(TITLE, ScalarType::String),
            published,
            ordinal,
            excerpt,
        ],
    );
    articles.insert_key_policy = InsertKeyPolicy::GeneratedOnly;
    articles.key_generation = Some(KeyGeneration::UuidV4);
    articles
        .unique_constraints
        .push(UniqueConstraintDeclaration::new(
            "publication_title",
            vec!["publication".into(), TITLE.into()],
        ));
    let mut unique = UniqueConstraintDeclaration::new("excerpt", vec!["excerpt".into()]);
    unique.null_policy = Some(UniqueNullPolicy::Distinct);
    articles.unique_constraints.push(unique);
    let mut permissions = TableDeclaration::new(
        "permissions",
        vec![
            FieldDefinition::new("id", ScalarType::String),
            FieldDefinition::new("allowed", ScalarType::Boolean),
        ],
    );
    let mut nullable = FieldDefinition::new("tag", ScalarType::String);
    nullable.nullable = true;
    nullable.default = Some(Value::Null);
    permissions.fields.push(nullable);
    let mut unique = UniqueConstraintDeclaration::new("tag", vec!["tag".into()]);
    unique.null_policy = Some(UniqueNullPolicy::NotDistinct);
    permissions.unique_constraints.push(unique);
    let mut counters = TableDeclaration::new(
        "counters",
        vec![
            FieldDefinition::new("id", ScalarType::Int64),
            FieldDefinition::new("value", ScalarType::Int64),
        ],
    );
    counters.key_generation = Some(KeyGeneration::Custom {
        handler: "next_counter".into(),
        output_type: ScalarType::Int64,
    });
    let mut schema = SchemaDeclaration::new(vec![
        publications,
        articles,
        permissions,
        counters,
        TableDeclaration::new("markers", vec![]),
    ]);
    schema.references.push(ReferenceDeclaration::new(
        "articles",
        "publication",
        "publications",
    ));
    schema.normalize().unwrap()
}

#[derive(Clone)]
struct Identity {
    admin: bool,
    owner: String,
}

struct Policy;
impl Authorization<Identity> for Policy {
    fn authorize<'a>(
        &'a self,
        context: QueryContext<'a, Identity>,
        _: Invocation,
        _: &'a Json,
    ) -> FunctionFuture<'a, ()> {
        Box::pin(async move {
            if context.identity.admin {
                return Ok(());
            }
            let permission = context
                .db
                .get("permissions", Value::String(context.identity.owner.clone()))
                .await?;
            if permission.is_some_and(|row| row.values["allowed"] == Value::Boolean(true)) {
                Ok(())
            } else {
                Err(Error::Forbidden)
            }
        })
    }
}

type MutationCall = for<'a> fn(MutationContext<'a, Identity>, &'a Json) -> FunctionFuture<'a, Json>;
struct MutationFn {
    call: MutationCall,
    invalid_result: bool,
}
impl FunctionContract for MutationFn {
    fn validate_input(&self, input: &Json) -> Result<(), Error> {
        if input.is_object() {
            Ok(())
        } else {
            Err(Error::InvalidInput("object input required".into()))
        }
    }
    fn validate_result(&self, result: &Json) -> Result<(), Error> {
        if self.invalid_result || !result.is_object() {
            Err(Error::InvalidInput("result contract rejected".into()))
        } else {
            Ok(())
        }
    }
}
impl Mutation<Identity> for MutationFn {
    fn call<'a>(
        &'a self,
        context: MutationContext<'a, Identity>,
        input: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        (self.call)(context, input)
    }
}

struct Articles;
impl FunctionContract for Articles {
    fn validate_input(&self, input: &Json) -> Result<(), Error> {
        if input.is_object() {
            Ok(())
        } else {
            Err(Error::InvalidInput("object input required".into()))
        }
    }
    fn validate_result(&self, result: &Json) -> Result<(), Error> {
        if result.is_array() {
            Ok(())
        } else {
            Err(Error::InvalidInput("array result required".into()))
        }
    }
}
impl Query<Identity> for Articles {
    fn call<'a>(
        &'a self,
        context: QueryContext<'a, Identity>,
        input: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        Box::pin(async move {
            let equals = input
                .get("publication")
                .map(|v| -> Result<_, Error> {
                    Ok(RecordValues::from([(
                        "publication".into(),
                        Value::String(
                            v.as_str()
                                .ok_or(Error::InvalidInput("publication must be a string".into()))?
                                .into(),
                        ),
                    )]))
                })
                .transpose()?
                .unwrap_or_default();
            let after = input
                .get("after")
                .map(|v| -> Result<_, Error> {
                    Ok(Value::Uuid(
                        *Uuid::parse_str(
                            v.as_str()
                                .ok_or(Error::InvalidInput("invalid cursor".into()))?,
                        )
                        .map_err(|_| Error::InvalidInput("invalid cursor".into()))?
                        .as_bytes(),
                    ))
                })
                .transpose()?;
            let limit = input
                .get("limit")
                .map(|v| {
                    v.as_u64()
                        .and_then(|v| u16::try_from(v).ok())
                        .ok_or(Error::InvalidInput("invalid limit".into()))
                })
                .transpose()?
                .unwrap_or(100);
            let rows = context
                .db
                .select(
                    "articles",
                    Select {
                        equals,
                        after,
                        limit,
                    },
                )
                .await?;
            Ok(Json::Array(rows.iter().map(article_json).collect()))
        })
    }
}

fn article_json(row: &Row) -> Json {
    let Value::Uuid(id) = row.values["id"] else {
        panic!("invalid UUID")
    };
    let Value::String(title) = &row.values[TITLE] else {
        panic!("invalid title")
    };
    let Value::Int64(ordinal) = row.values["ordinal"] else {
        panic!("invalid ordinal")
    };
    json!({ "id": Uuid::from_bytes(id), "title": title, "ordinal": ordinal.to_string(),
        "published": row.values["published"] == Value::Boolean(true), "version": row.version,
        "excerpt": match &row.values["excerpt"] { Value::String(v) => Some(v), Value::Null => None, _ => panic!("invalid excerpt") },
    })
}

fn text(input: &Json, name: &str) -> Result<String, Error> {
    input
        .get(name)
        .and_then(Json::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::InvalidInput(format!("{name} must be a string")))
}
fn key(input: &Json) -> Result<Value, Error> {
    Ok(Value::Uuid(
        *Uuid::parse_str(&text(input, "id")?)
            .map_err(|_| Error::InvalidInput("invalid ID".into()))?
            .as_bytes(),
    ))
}
fn version(input: &Json) -> Result<i64, Error> {
    input
        .get("version")
        .and_then(Json::as_i64)
        .ok_or_else(|| Error::InvalidInput("version required".into()))
}

fn bundle<'a>(context: MutationContext<'a, Identity>, input: &'a Json) -> FunctionFuture<'a, Json> {
    Box::pin(async move {
        let publication = text(input, "publication")?;
        context
            .db
            .insert(
                "publications",
                RecordValues::from([
                    ("slug".into(), Value::String(publication.clone())),
                    ("name".into(), Value::String("Daily".into())),
                ]),
            )
            .await?;
        let first = context
            .db
            .insert(
                "articles",
                RecordValues::from([
                    ("publication".into(), Value::String(publication.clone())),
                    (TITLE.into(), Value::String(text(input, "title")?)),
                ]),
            )
            .await?;
        let second = context
            .db
            .insert(
                "articles",
                RecordValues::from([
                    ("publication".into(), Value::String(publication)),
                    (TITLE.into(), Value::String(text(input, "second")?)),
                ]),
            )
            .await?;
        Ok(json!({"first": article_json(&first), "second": article_json(&second)}))
    })
}
fn article<'a>(
    context: MutationContext<'a, Identity>,
    input: &'a Json,
) -> FunctionFuture<'a, Json> {
    Box::pin(async move {
        let mut values = RecordValues::from([
            (
                "publication".into(),
                Value::String(text(input, "publication")?),
            ),
            (TITLE.into(), Value::String(text(input, "title")?)),
        ]);
        if let Some(excerpt) = input.get("excerpt") {
            values.insert(
                "excerpt".into(),
                if excerpt.is_null() {
                    Value::Null
                } else {
                    Value::String(text(input, "excerpt")?)
                },
            );
        }
        if input.get("id").is_some() {
            values.insert("id".into(), key(input)?);
        }
        Ok(article_json(&context.db.insert("articles", values).await?))
    })
}
fn edit<'a>(context: MutationContext<'a, Identity>, input: &'a Json) -> FunctionFuture<'a, Json> {
    Box::pin(async move {
        let mut changes = RecordValues::from([("published".into(), Value::Boolean(true))]);
        if input.get("title").is_some() {
            changes.insert(TITLE.into(), Value::String(text(input, "title")?));
        }
        if let Some(excerpt) = input.get("excerpt") {
            changes.insert(
                "excerpt".into(),
                if excerpt.is_null() {
                    Value::Null
                } else {
                    Value::String(text(input, "excerpt")?)
                },
            );
        }
        if input.get("assign_key").is_some() {
            changes.insert("id".into(), key(input)?);
        }
        Ok(article_json(
            &context
                .db
                .update("articles", key(input)?, version(input)?, changes)
                .await?,
        ))
    })
}
fn delete<'a>(context: MutationContext<'a, Identity>, input: &'a Json) -> FunctionFuture<'a, Json> {
    Box::pin(async move {
        let table = text(input, "table")?;
        let key = if table == "articles" {
            key(input)?
        } else {
            Value::String(text(input, "id")?)
        };
        context.db.delete(&table, key, version(input)?).await?;
        Ok(json!({"deleted": true}))
    })
}
fn permission<'a>(
    context: MutationContext<'a, Identity>,
    input: &'a Json,
) -> FunctionFuture<'a, Json> {
    Box::pin(async move {
        if !context.identity.admin {
            return Err(Error::Forbidden);
        }
        context
            .db
            .insert(
                "permissions",
                RecordValues::from([
                    ("id".into(), Value::String(text(input, "id")?)),
                    ("allowed".into(), Value::Boolean(true)),
                ]),
            )
            .await?;
        Ok(json!({"granted": true}))
    })
}
fn revoke<'a>(context: MutationContext<'a, Identity>, input: &'a Json) -> FunctionFuture<'a, Json> {
    Box::pin(async move {
        if !context.identity.admin {
            return Err(Error::Forbidden);
        }
        context
            .db
            .update(
                "permissions",
                Value::String(text(input, "id")?),
                1,
                RecordValues::from([("allowed".into(), Value::Boolean(false))]),
            )
            .await?;
        Ok(json!({"revoked": true}))
    })
}
fn counter<'a>(context: MutationContext<'a, Identity>, _: &'a Json) -> FunctionFuture<'a, Json> {
    Box::pin(async move {
        let row = context
            .db
            .insert(
                "counters",
                RecordValues::from([("value".into(), Value::Int64(i64::MIN))]),
            )
            .await?;
        let Value::Int64(id) = row.values["id"] else {
            panic!("invalid key")
        };
        Ok(json!({"id": id.to_string()}))
    })
}
struct CounterGenerator;
impl KeyGenerator<Identity> for CounterGenerator {
    fn generate<'a>(
        &'a self,
        context: MutationContext<'a, Identity>,
        _: &'a str,
    ) -> FunctionFuture<'a, Value> {
        Box::pin(async move {
            let marker = context.db.insert("markers", RecordValues::new()).await?;
            let Value::Uuid(id) = marker.values["id"] else {
                panic!("invalid marker")
            };
            assert_eq!(Uuid::from_bytes(id).get_version_num(), 7);
            Ok(Value::Int64(i64::MAX))
        })
    }
}

fn registry() -> Registry<Identity> {
    let mut registry = Registry::new();
    registry.query("articles", Articles, Policy).unwrap();
    for (name, call) in [
        ("bundle", bundle as MutationCall),
        ("article", article),
        ("edit", edit),
        ("delete", delete),
        ("permission", permission),
        ("revoke", revoke),
        ("counter", counter),
    ] {
        registry
            .mutation(
                name,
                MutationFn {
                    call,
                    invalid_result: false,
                },
                Policy,
            )
            .unwrap();
    }
    registry
        .mutation(
            "bad_result",
            MutationFn {
                call: article,
                invalid_result: true,
            },
            Policy,
        )
        .unwrap();
    registry
        .key_generator("next_counter", CounterGenerator)
        .unwrap();
    registry
}

fn url() -> String {
    std::env::var("HULLADB_TEST_URL")
        .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into())
}
async fn fixture() -> (ManagedStore, Registry<Identity>, Context<Identity>) {
    let layout =
        ManagedSchema::new(format!("managed_{}", Uuid::now_v7().simple()), schema()).unwrap();
    let mut store = ManagedStore::connect(&url(), layout).await.unwrap();
    store.initialize().await.unwrap();
    let context = Context {
        scope: Uuid::now_v7(),
        principal: Uuid::now_v7(),
        authorization_scope: Uuid::now_v7(),
        identity: Identity {
            admin: true,
            owner: "reader".into(),
        },
    };
    store.create_scope(context.scope).await.unwrap();
    (store, registry(), context)
}
async fn admin(sql: &str) {
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    client.batch_execute(sql).await.unwrap();
    connection.abort();
}
async fn cleanup(store: &ManagedStore) {
    admin(&format!(
        "DROP SCHEMA \"{}\" CASCADE",
        store.schema().namespace()
    ))
    .await;
}
fn bundle_input() -> Json {
    json!({"publication": "daily", "title": "First", "second": "Second"})
}
async fn snapshot(
    store: &mut ManagedStore,
    registry: &Registry<Identity>,
    context: &Context<Identity>,
) -> hulladb_postgres::managed::Snapshot {
    store
        .query(registry, context, "articles", json!({}))
        .await
        .unwrap()
}

#[test]
fn unsafe_storage_names_and_duplicate_function_identities_are_rejected() {
    for namespace in [
        "public",
        "pg_catalog",
        "bad\"; DROP SCHEMA public",
        "",
        &"a".repeat(64),
    ] {
        assert!(ManagedSchema::new(namespace, schema()).is_err());
    }
    let mut registry = registry();
    assert!(registry.query("bundle", Articles, Policy).is_err());
    assert!(
        registry
            .key_generator("next_counter", CounterGenerator)
            .is_err()
    );
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn user_tables_multi_operation_defaults_constraints_and_receipt_recovery() {
    let (mut store, registry, context) = fixture().await;
    let request = Uuid::now_v7();
    let receipt = store
        .mutate(&registry, &context, request, "bundle", bundle_input())
        .await
        .unwrap();
    assert_eq!(receipt.revision.sequence, 1);
    assert_eq!(receipt.result["first"]["ordinal"], i64::MAX.to_string());
    assert_eq!(receipt.result["first"]["published"], false);
    assert_eq!(receipt.result["first"]["excerpt"], Json::Null);
    let id = Uuid::parse_str(receipt.result["first"]["id"].as_str().unwrap()).unwrap();
    assert_eq!(id.get_version_num(), 4);
    let mut other = ManagedStore::connect(&url(), store.schema().clone())
        .await
        .unwrap();
    other.initialize().await.unwrap();
    assert_eq!(
        other.receipt(&context, request).await.unwrap(),
        Some(receipt.clone())
    );
    assert_eq!(
        other
            .mutate(&registry, &context, request, "bundle", bundle_input())
            .await
            .unwrap(),
        receipt
    );
    assert!(matches!(
        other
            .mutate(&registry, &context, request, "article", json!({}))
            .await,
        Err(Error::RequestIdReused)
    ));
    assert!(matches!(
        other
            .mutate(
                &registry,
                &context,
                request,
                "bundle",
                json!({"different": true})
            )
            .await,
        Err(Error::RequestIdReused)
    ));
    let view = snapshot(&mut store, &registry, &context).await;
    assert_eq!(view.source, receipt.source);
    assert_eq!(view.revision, receipt.revision);
    assert_eq!(view.result.as_array().unwrap().len(), 2);
    assert_eq!(view.tables, ["articles".into()].into());
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn uniqueness_and_result_validation_roll_back_all_operations_revision_and_receipt() {
    let (mut store, registry, context) = fixture().await;
    let request = Uuid::now_v7();
    let input = json!({"publication": "daily", "title": "Same", "second": "Same"});
    assert!(matches!(
        store
            .mutate(&registry, &context, request, "bundle", input)
            .await,
        Err(Error::Conflict)
    ));
    assert!(store.receipt(&context, request).await.unwrap().is_none());
    assert_eq!(
        snapshot(&mut store, &registry, &context)
            .await
            .revision
            .sequence,
        0
    );
    // The first operation's publication must also have rolled back.
    store
        .mutate(&registry, &context, request, "bundle", bundle_input())
        .await
        .unwrap();
    let before = snapshot(&mut store, &registry, &context).await;
    let request = Uuid::now_v7();
    assert!(matches!(
        store
            .mutate(
                &registry,
                &context,
                request,
                "bad_result",
                json!({"publication": "daily", "title": "Rejected"})
            )
            .await,
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(snapshot(&mut store, &registry, &context).await, before);
    assert!(store.receipt(&context, request).await.unwrap().is_none());
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn schema_constraints_and_versions_cannot_be_bypassed() {
    let (mut store, registry, context) = fixture().await;
    let receipt = store
        .mutate(
            &registry,
            &context,
            Uuid::now_v7(),
            "bundle",
            bundle_input(),
        )
        .await
        .unwrap();
    let id = receipt.result["first"]["id"].as_str().unwrap();
    for (operation, input) in [
        (
            "article",
            json!({"publication": "missing", "title": "Orphan"}),
        ),
        (
            "article",
            json!({"publication": "daily", "title": "Explicit", "id": id}),
        ),
        ("edit", json!({"id": id, "version": 1, "assign_key": true})),
        (
            "delete",
            json!({"table": "publications", "id": "daily", "version": 1}),
        ),
    ] {
        let request = Uuid::now_v7();
        assert!(
            store
                .mutate(&registry, &context, request, operation, input)
                .await
                .is_err()
        );
        assert!(store.receipt(&context, request).await.unwrap().is_none());
        assert_eq!(
            snapshot(&mut store, &registry, &context)
                .await
                .revision
                .sequence,
            1
        );
    }
    let update = store
        .mutate(
            &registry,
            &context,
            Uuid::now_v7(),
            "edit",
            json!({"id": id, "version": 1, "excerpt": "Summary"}),
        )
        .await
        .unwrap();
    assert_eq!(update.result["version"], 2);
    assert_eq!(update.result["title"], "First");
    assert_eq!(update.result["excerpt"], "Summary");
    let update = store
        .mutate(
            &registry,
            &context,
            Uuid::now_v7(),
            "edit",
            json!({"id": id, "version": 2, "excerpt": null}),
        )
        .await
        .unwrap();
    assert_eq!(update.result["excerpt"], Json::Null);
    assert!(matches!(
        store
            .mutate(
                &registry,
                &context,
                Uuid::now_v7(),
                "edit",
                json!({"id": id, "version": 1})
            )
            .await,
        Err(Error::Conflict)
    ));
    store
        .mutate(
            &registry,
            &context,
            Uuid::now_v7(),
            "delete",
            json!({"table": "articles", "id": id, "version": 3}),
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .mutate(
                &registry,
                &context,
                Uuid::now_v7(),
                "edit",
                json!({"id": id, "version": 3})
            )
            .await,
        Err(Error::NotFound)
    ));
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn tracked_empty_reads_permissions_and_identity_scoped_recovery() {
    let (mut store, registry, admin_context) = fixture().await;
    store
        .mutate(
            &registry,
            &admin_context,
            Uuid::now_v7(),
            "permission",
            json!({"id": "reader"}),
        )
        .await
        .unwrap();
    let user = Context {
        principal: Uuid::now_v7(),
        authorization_scope: Uuid::now_v7(),
        identity: Identity {
            admin: false,
            owner: "reader".into(),
        },
        ..admin_context.clone()
    };
    let initial = snapshot(&mut store, &registry, &user).await;
    assert_eq!(initial.result, json!([]));
    assert_eq!(
        initial.tables,
        ["permissions".into(), "articles".into()].into()
    );
    let request = Uuid::now_v7();
    let receipt = store
        .mutate(&registry, &user, request, "bundle", bundle_input())
        .await
        .unwrap();
    assert!(
        store
            .receipt(&admin_context, request)
            .await
            .unwrap()
            .is_none()
    );
    let changed = Context {
        authorization_scope: Uuid::now_v7(),
        ..user.clone()
    };
    assert!(matches!(
        store.receipt(&changed, request).await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        store
            .mutate(&registry, &changed, request, "bundle", bundle_input())
            .await,
        Err(Error::Forbidden)
    ));
    let later = snapshot(&mut store, &registry, &user).await;
    assert!(later.revision.sequence > initial.revision.sequence);
    assert_eq!(later.revision, receipt.revision);
    store
        .mutate(
            &registry,
            &admin_context,
            Uuid::now_v7(),
            "revoke",
            json!({"id": "reader"}),
        )
        .await
        .unwrap();
    assert!(matches!(
        store.query(&registry, &user, "articles", json!({})).await,
        Err(Error::Forbidden)
    ));
    // Revocation blocks new executions, not delivery of the original response.
    assert_eq!(
        store.receipt(&user, request).await.unwrap(),
        Some(receipt.clone())
    );
    assert_eq!(
        store
            .mutate(&registry, &user, request, "bundle", bundle_input())
            .await
            .unwrap(),
        receipt
    );
    assert!(matches!(
        store
            .mutate(
                &registry,
                &user,
                Uuid::now_v7(),
                "article",
                json!({"publication": "daily", "title": "Denied"})
            )
            .await,
        Err(Error::Forbidden)
    ));
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn concurrent_submissions_commit_once_and_stale_updates_have_one_winner() {
    let (mut store, registry, context) = fixture().await;
    let mut other = ManagedStore::connect(&url(), store.schema().clone())
        .await
        .unwrap();
    other.initialize().await.unwrap();
    let request = Uuid::now_v7();
    let (a, b) = tokio::join!(
        store.mutate(&registry, &context, request, "bundle", bundle_input()),
        other.mutate(&registry, &context, request, "bundle", bundle_input()),
    );
    let receipt = a.unwrap();
    assert_eq!(receipt, b.unwrap());
    let input = json!({"id": receipt.result["first"]["id"], "version": 1});
    let request_a = Uuid::now_v7();
    let request_b = Uuid::now_v7();
    let (a, b) = tokio::join!(
        store.mutate(&registry, &context, request_a, "edit", input.clone()),
        other.mutate(&registry, &context, request_b, "edit", input),
    );
    let rejected = match (a, b) {
        (Ok(_), Err(Error::Conflict)) => request_b,
        (Err(Error::Conflict), Ok(_)) => request_a,
        other => panic!("unexpected outcomes: {other:?}"),
    };
    assert!(store.receipt(&context, rejected).await.unwrap().is_none());
    assert_eq!(
        snapshot(&mut store, &registry, &context)
            .await
            .revision
            .sequence,
        2
    );
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn scopes_keyset_filters_null_uniqueness_and_reference_isolation() {
    let (mut store, registry, context) = fixture().await;
    let other = Context {
        scope: Uuid::now_v7(),
        ..context.clone()
    };
    store.create_scope(other.scope).await.unwrap();
    store
        .mutate(
            &registry,
            &context,
            Uuid::now_v7(),
            "bundle",
            bundle_input(),
        )
        .await
        .unwrap();
    assert_eq!(
        snapshot(&mut store, &registry, &other).await.result,
        json!([])
    );
    // A reference cannot resolve to a row in another scope.
    assert!(matches!(
        store
            .mutate(
                &registry,
                &other,
                Uuid::now_v7(),
                "article",
                json!({"publication": "daily", "title": "Orphan"})
            )
            .await,
        Err(Error::ReferenceViolation)
    ));
    store
        .mutate(&registry, &other, Uuid::now_v7(), "bundle", bundle_input())
        .await
        .unwrap();
    let first = store
        .query(&registry, &context, "articles", json!({"limit": 1}))
        .await
        .unwrap();
    let second = store
        .query(
            &registry,
            &context,
            "articles",
            json!({"limit": 1, "after": first.result[0]["id"]}),
        )
        .await
        .unwrap();
    assert_eq!(second.result.as_array().unwrap().len(), 1);
    assert_ne!(first.result[0]["id"], second.result[0]["id"]);
    assert_eq!(
        store
            .query(
                &registry,
                &context,
                "articles",
                json!({"publication": "missing"})
            )
            .await
            .unwrap()
            .result,
        json!([])
    );
    assert!(matches!(
        store
            .query(&registry, &context, "articles", json!({"limit": 0}))
            .await,
        Err(Error::InvalidInput(_))
    ));
    for title in ["Third", "Fourth"] {
        let result = store
            .mutate(
                &registry,
                &context,
                Uuid::now_v7(),
                "article",
                json!({"publication": "daily", "title": title, "excerpt": "Unique"}),
            )
            .await;
        if title == "Third" {
            assert!(result.is_ok());
        } else {
            assert!(matches!(result, Err(Error::Conflict)));
        }
    }
    store
        .mutate(
            &registry,
            &context,
            Uuid::now_v7(),
            "permission",
            json!({"id": "one"}),
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .mutate(
                &registry,
                &context,
                Uuid::now_v7(),
                "permission",
                json!({"id": "two"})
            )
            .await,
        Err(Error::Conflict)
    ));
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn custom_generators_share_the_transaction_and_validate_generated_values() {
    let (mut store, registry, context) = fixture().await;
    let result = store
        .mutate(&registry, &context, Uuid::now_v7(), "counter", json!({}))
        .await
        .unwrap();
    assert_eq!(result.result["id"], i64::MAX.to_string());
    assert_eq!(result.revision.sequence, 1);
    let request = Uuid::now_v7();
    assert!(matches!(
        store
            .mutate(&registry, &context, request, "counter", json!({}))
            .await,
        Err(Error::Conflict)
    ));
    // Counter conflict rolls back the generator's marker and the receipt.
    assert!(store.receipt(&context, request).await.unwrap().is_none());
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let count: i64 = client
        .query_one(
            &format!("SELECT count(*) FROM \"{}\".t4", store.schema().namespace()),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 1);
    connection.abort();
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn receipt_failure_rolls_back_data_and_revision_and_same_request_can_retry() {
    let (mut store, registry, context) = fixture().await;
    let namespace = store.schema().namespace().to_owned();
    admin(&format!("CREATE FUNCTION \"{namespace}\".fail_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected receipt failure'; END $$; CREATE TRIGGER fail_receipt BEFORE INSERT ON \"{namespace}\".receipts FOR EACH ROW EXECUTE FUNCTION \"{namespace}\".fail_receipt();")).await;
    let request = Uuid::now_v7();
    let failed = store
        .mutate(&registry, &context, request, "bundle", bundle_input())
        .await;
    admin(&format!("DROP TRIGGER fail_receipt ON \"{namespace}\".receipts; DROP FUNCTION \"{namespace}\".fail_receipt();")).await;
    assert!(matches!(failed, Err(Error::Database(_))));
    assert!(store.receipt(&context, request).await.unwrap().is_none());
    assert_eq!(
        snapshot(&mut store, &registry, &context)
            .await
            .revision
            .sequence,
        0
    );
    assert_eq!(
        store
            .mutate(&registry, &context, request, "bundle", bundle_input())
            .await
            .unwrap()
            .revision
            .sequence,
        1
    );
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn mismatched_schemas_and_wrong_dispatch_do_not_modify_existing_data() {
    let (mut store, registry, context) = fixture().await;
    store
        .mutate(
            &registry,
            &context,
            Uuid::now_v7(),
            "bundle",
            bundle_input(),
        )
        .await
        .unwrap();
    let before = snapshot(&mut store, &registry, &context).await;
    let incompatible = SchemaDeclaration::new(vec![TableDeclaration::new("unrelated", vec![])])
        .normalize()
        .unwrap();
    let mut other = ManagedStore::connect(
        &url(),
        ManagedSchema::new(store.schema().namespace(), incompatible).unwrap(),
    )
    .await
    .unwrap();
    assert!(matches!(
        other.initialize().await,
        Err(Error::SchemaMismatch)
    ));
    assert!(other.create_scope(Uuid::now_v7()).await.is_err());
    assert!(matches!(
        store.query(&registry, &context, "bundle", json!({})).await,
        Err(Error::WrongFunctionKind)
    ));
    assert!(matches!(
        store
            .mutate(&registry, &context, Uuid::now_v7(), "articles", json!({}))
            .await,
        Err(Error::WrongFunctionKind)
    ));
    assert!(matches!(
        store.query(&registry, &context, "missing", json!({})).await,
        Err(Error::UnknownFunction(_))
    ));
    assert!(matches!(
        store
            .mutate(&registry, &context, Uuid::now_v7(), "article", Json::Null)
            .await,
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(snapshot(&mut store, &registry, &context).await, before);
    cleanup(&store).await;
}

struct RepeatedReads {
    started: Arc<tokio::sync::Notify>,
    resume: Arc<tokio::sync::Notify>,
}
impl FunctionContract for RepeatedReads {
    fn validate_input(&self, input: &Json) -> Result<(), Error> {
        Articles.validate_input(input)
    }
    fn validate_result(&self, result: &Json) -> Result<(), Error> {
        if result.is_object() {
            Ok(())
        } else {
            Err(Error::CorruptData)
        }
    }
}
impl Query<Identity> for RepeatedReads {
    fn call<'a>(
        &'a self,
        context: QueryContext<'a, Identity>,
        _: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        Box::pin(async move {
            let first = context.db.select("articles", Select::default()).await?;
            self.started.notify_one();
            self.resume.notified().await;
            let second = context.db.select("articles", Select::default()).await?;
            Ok(json!({"first": first.len(), "second": second.len()}))
        })
    }
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn query_function_reads_and_revision_share_one_consistent_snapshot() {
    let (mut store, mut registry, context) = fixture().await;
    store
        .mutate(
            &registry,
            &context,
            Uuid::now_v7(),
            "bundle",
            bundle_input(),
        )
        .await
        .unwrap();
    let mut other = ManagedStore::connect(&url(), store.schema().clone())
        .await
        .unwrap();
    other.initialize().await.unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    registry
        .query(
            "repeated",
            RepeatedReads {
                started: started.clone(),
                resume: resume.clone(),
            },
            Policy,
        )
        .unwrap();
    let (view, receipt) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(
            store.query(&registry, &context, "repeated", json!({})),
            async {
                started.notified().await;
                let receipt = other
                    .mutate(
                        &registry,
                        &context,
                        Uuid::now_v7(),
                        "article",
                        json!({"publication": "daily", "title": "Later"}),
                    )
                    .await;
                resume.notify_one();
                receipt
            }
        )
    })
    .await
    .unwrap();
    let view = view.unwrap();
    assert_eq!(view.result, json!({"first": 2, "second": 2}));
    assert_eq!(view.revision.sequence, 1);
    assert_eq!(receipt.unwrap().revision.sequence, 2);
    assert_eq!(
        snapshot(&mut store, &registry, &context)
            .await
            .result
            .as_array()
            .unwrap()
            .len(),
        3
    );
    cleanup(&store).await;
}

struct CountedMutation(Arc<AtomicUsize>);
struct CountedPolicy(Arc<AtomicUsize>);
impl Authorization<Identity> for CountedPolicy {
    fn authorize<'a>(
        &'a self,
        context: QueryContext<'a, Identity>,
        invocation: Invocation,
        input: &'a Json,
    ) -> FunctionFuture<'a, ()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Policy.authorize(context, invocation, input)
    }
}
impl FunctionContract for CountedMutation {
    fn validate_input(&self, input: &Json) -> Result<(), Error> {
        Articles.validate_input(input)
    }
    fn validate_result(&self, result: &Json) -> Result<(), Error> {
        if result.is_object() {
            Ok(())
        } else {
            Err(Error::CorruptData)
        }
    }
}
impl Mutation<Identity> for CountedMutation {
    fn call<'a>(
        &'a self,
        context: MutationContext<'a, Identity>,
        input: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        self.0.fetch_add(1, Ordering::SeqCst);
        bundle(context, input)
    }
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn recovered_receipts_never_rerun_the_registered_handler_or_authorization() {
    let (mut store, mut registry, context) = fixture().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let guards = Arc::new(AtomicUsize::new(0));
    registry
        .mutation(
            "counted",
            CountedMutation(calls.clone()),
            CountedPolicy(guards.clone()),
        )
        .unwrap();
    let request = Uuid::now_v7();
    let first = store
        .mutate(&registry, &context, request, "counted", bundle_input())
        .await
        .unwrap();
    assert_eq!(
        store.receipt(&context, request).await.unwrap(),
        Some(first.clone())
    );
    assert_eq!(
        store
            .mutate(&registry, &context, request, "counted", bundle_input())
            .await
            .unwrap(),
        first
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(guards.load(Ordering::SeqCst), 1);
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn receipt_input_binding_survives_reconnect_without_retaining_submitted_payload() {
    let (mut store, registry, context) = fixture().await;
    let request = Uuid::now_v7();
    let mut input = bundle_input();
    // The handler ignores this field. It must still be bound to the invocation,
    // but it must not become a second retained copy of the submitted payload.
    input["extra"] = json!({"private": "submitted-only-secret".repeat(4096), "zero": -0.0});
    let original = store
        .mutate(&registry, &context, request, "bundle", input.clone())
        .await
        .unwrap();
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let saved: Json = client
        .query_one(
            &format!(
                "SELECT to_jsonb(r) FROM \"{}\".receipts r WHERE request_id=$1",
                store.schema().namespace()
            ),
            &[&request],
        )
        .await
        .unwrap()
        .get(0);
    assert!(saved.get("input").is_none());
    assert!(!saved.to_string().contains("submitted-only-secret"));
    let mut reconnected = ManagedStore::connect(&url(), store.schema().clone())
        .await
        .unwrap();
    reconnected.verify().await.unwrap();
    input["extra"]["zero"] = json!(0.0);
    assert_eq!(
        reconnected
            .mutate(&registry, &context, request, "bundle", input.clone())
            .await
            .unwrap(),
        original
    );
    input["extra"]["zero"] = json!(0);
    assert!(matches!(
        reconnected
            .mutate(&registry, &context, request, "bundle", input)
            .await,
        Err(Error::RequestIdReused)
    ));
    assert_eq!(
        reconnected.receipt(&context, request).await.unwrap(),
        Some(original)
    );
    drop(reconnected);
    connection.abort();
    cleanup(&store).await;
}

struct WrongGenerator;
impl KeyGenerator<Identity> for WrongGenerator {
    fn generate<'a>(
        &'a self,
        context: MutationContext<'a, Identity>,
        _: &'a str,
    ) -> FunctionFuture<'a, Value> {
        Box::pin(async move {
            context.db.insert("markers", RecordValues::new()).await?;
            Ok(Value::Boolean(false))
        })
    }
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn missing_or_invalid_custom_generators_leave_no_partial_effects() {
    let (mut store, _, context) = fixture().await;
    let mut registry = Registry::new();
    registry
        .mutation(
            "counter",
            MutationFn {
                call: counter,
                invalid_result: false,
            },
            Policy,
        )
        .unwrap();
    let request = Uuid::now_v7();
    assert!(matches!(
        store
            .mutate(&registry, &context, request, "counter", json!({}))
            .await,
        Err(Error::MissingGenerator(_))
    ));
    registry
        .key_generator("next_counter", WrongGenerator)
        .unwrap();
    assert!(matches!(
        store
            .mutate(&registry, &context, request, "counter", json!({}))
            .await,
        Err(Error::Validation(_))
    ));
    assert!(store.receipt(&context, request).await.unwrap().is_none());
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let namespace = store.schema().namespace();
    let row = client.query_one(&format!("SELECT (SELECT count(*) FROM \"{namespace}\".t4), (SELECT sequence FROM \"{namespace}\".scopes WHERE id=$1)"), &[&context.scope]).await.unwrap();
    assert_eq!(row.get::<_, i64>(0), 0);
    assert_eq!(row.get::<_, i64>(1), 0);
    connection.abort();
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn explicit_commit_rejection_is_a_known_failure_and_rolls_back_everything() {
    let (mut store, registry, context) = fixture().await;
    let namespace = store.schema().namespace().to_owned();
    admin(&format!("CREATE FUNCTION \"{namespace}\".fail_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected deferred commit failure' USING ERRCODE = '23514'; END $$; CREATE CONSTRAINT TRIGGER fail_commit AFTER INSERT ON \"{namespace}\".receipts DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION \"{namespace}\".fail_commit();")).await;
    let request = Uuid::now_v7();
    let failed = store
        .mutate(&registry, &context, request, "bundle", bundle_input())
        .await;
    admin(&format!("DROP TRIGGER fail_commit ON \"{namespace}\".receipts; DROP FUNCTION \"{namespace}\".fail_commit();")).await;
    assert!(matches!(failed, Err(Error::Database(_))));
    assert!(store.receipt(&context, request).await.unwrap().is_none());
    let view = snapshot(&mut store, &registry, &context).await;
    assert_eq!(view.revision.sequence, 0);
    assert_eq!(view.result, json!([]));
    assert_eq!(
        store
            .mutate(&registry, &context, request, "bundle", bundle_input())
            .await
            .unwrap()
            .revision
            .sequence,
        1
    );
    cleanup(&store).await;
}

// A later deployment may replace both validators and the handler. Confirmation
// must not evaluate either the old or new application code.
struct ReplacedMutation;
impl FunctionContract for ReplacedMutation {
    fn validate_input(&self, _: &Json) -> Result<(), Error> {
        Err(Error::InvalidInput("replacement input contract".into()))
    }
    fn validate_result(&self, _: &Json) -> Result<(), Error> {
        panic!("saved response must not run a replacement output validator")
    }
}
impl Mutation<Identity> for ReplacedMutation {
    fn call<'a>(
        &'a self,
        _: MutationContext<'a, Identity>,
        _: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        panic!("saved response must not run a replacement handler")
    }
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn confirmation_survives_removed_or_replaced_declarations_without_revalidation() {
    let (mut store, registry, context) = fixture().await;
    let request = Uuid::now_v7();
    let input = bundle_input();
    let original = store
        .mutate(&registry, &context, request, "bundle", input.clone())
        .await
        .unwrap();
    drop(registry);
    assert_eq!(
        store.receipt(&context, request).await.unwrap(),
        Some(original.clone())
    );
    let mut replacement = Registry::new();
    // Even after removal, duplicate delivery is not a new function execution.
    assert_eq!(
        store
            .mutate(&replacement, &context, request, "bundle", input.clone())
            .await
            .unwrap(),
        original
    );
    assert!(matches!(
        store
            .mutate(
                &replacement,
                &context,
                Uuid::now_v7(),
                "bundle",
                input.clone()
            )
            .await,
        Err(Error::UnknownFunction(_))
    ));
    replacement
        .mutation("bundle", ReplacedMutation, Policy)
        .unwrap();
    assert_eq!(
        store
            .mutate(&replacement, &context, request, "bundle", input.clone())
            .await
            .unwrap(),
        original
    );
    assert!(matches!(
        store
            .mutate(&replacement, &context, Uuid::now_v7(), "bundle", input)
            .await,
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        store
            .mutate(
                &replacement,
                &context,
                request,
                "bundle",
                json!({"changed": true})
            )
            .await,
        Err(Error::RequestIdReused)
    ));
    // Changing an operation name or its kind cannot bypass the original binding.
    replacement.query("bundle_query", Articles, Policy).unwrap();
    assert!(matches!(
        store
            .mutate(
                &replacement,
                &context,
                request,
                "bundle_query",
                bundle_input()
            )
            .await,
        Err(Error::RequestIdReused)
    ));
    assert!(matches!(
        store
            .mutate(
                &replacement,
                &context,
                Uuid::now_v7(),
                "bundle_query",
                json!({})
            )
            .await,
        Err(Error::WrongFunctionKind)
    ));
    cleanup(&store).await;
}

fn serializable(attempts: u16) -> hulladb_postgres::managed::MutationConcurrency {
    hulladb_postgres::managed::MutationConcurrency::Serializable(
        hulladb_postgres::managed::RetryOptions {
            max_attempts: attempts,
            timeout: std::time::Duration::from_secs(3),
            backoff: std::time::Duration::from_millis(1),
        },
    )
}

struct OverlappingMutation {
    barrier: Arc<tokio::sync::Barrier>,
    calls: Arc<AtomicUsize>,
    enforce_limit: bool,
}
impl FunctionContract for OverlappingMutation {
    fn validate_input(&self, input: &Json) -> Result<(), Error> {
        Articles.validate_input(input)
    }
    fn validate_result(&self, result: &Json) -> Result<(), Error> {
        if result.is_object() {
            Ok(())
        } else {
            Err(Error::CorruptData)
        }
    }
}
impl Mutation<Identity> for OverlappingMutation {
    fn call<'a>(
        &'a self,
        context: MutationContext<'a, Identity>,
        input: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        Box::pin(async move {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst);
            let count = if self.enforce_limit {
                context
                    .db
                    .select("articles", Select::default())
                    .await?
                    .len()
            } else {
                0
            };
            // Two first attempts must reach this together, before either commits.
            // A scope-wide handler lock cannot pass this gate.
            if attempt < 2 {
                self.barrier.wait().await;
            }
            if self.enforce_limit {
                if count >= 3 {
                    return Err(Error::Conflict);
                }
                article(context, input).await
            } else {
                context.db.insert("markers", RecordValues::new()).await?;
                Ok(json!({"inserted": true}))
            }
        })
    }
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn default_mutations_overlap_in_one_scope_and_retry_each_request_once() {
    let (mut store, mut registry, context) = fixture().await;
    let mut other = ManagedStore::connect(&url(), store.schema().clone())
        .await
        .unwrap();
    other.verify().await.unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    registry
        .mutation(
            "overlap",
            OverlappingMutation {
                barrier: Arc::new(tokio::sync::Barrier::new(2)),
                calls: calls.clone(),
                enforce_limit: false,
            },
            Policy,
        )
        .unwrap();
    let request_a = Uuid::now_v7();
    let request_b = Uuid::now_v7();
    let (a, b) = tokio::join!(
        store.mutate(&registry, &context, request_a, "overlap", json!({})),
        other.mutate(&registry, &context, request_b, "overlap", json!({})),
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(
        [a.revision.sequence, b.revision.sequence]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        [1, 2].into()
    );
    assert!(calls.load(Ordering::SeqCst) >= 3);
    assert_eq!(store.receipt(&context, request_a).await.unwrap(), Some(a));
    assert_eq!(other.receipt(&context, request_b).await.unwrap(), Some(b));
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let count: i64 = client
        .query_one(
            &format!("SELECT count(*) FROM \"{}\".t4", store.schema().namespace()),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 2);
    connection.abort();
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn serializable_predicate_invariants_survive_concurrent_writes_and_reauthorization() {
    let (mut store, mut registry, context) = fixture().await;
    store
        .mutate(
            &registry,
            &context,
            Uuid::now_v7(),
            "bundle",
            bundle_input(),
        )
        .await
        .unwrap();
    let mut other = ManagedStore::connect(&url(), store.schema().clone())
        .await
        .unwrap();
    other.verify().await.unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    registry
        .mutation_with_concurrency(
            "reserve",
            OverlappingMutation {
                barrier: Arc::new(tokio::sync::Barrier::new(2)),
                calls: calls.clone(),
                enforce_limit: true,
            },
            Policy,
            serializable(6),
        )
        .unwrap();
    let request_a = Uuid::now_v7();
    let request_b = Uuid::now_v7();
    let (a, b) = tokio::join!(
        store.mutate(
            &registry,
            &context,
            request_a,
            "reserve",
            json!({"publication":"daily","title":"A"})
        ),
        other.mutate(
            &registry,
            &context,
            request_b,
            "reserve",
            json!({"publication":"daily","title":"B"})
        ),
    );
    let rejected = match (a, b) {
        (Ok(_), Err(Error::Conflict)) => request_b,
        (Err(Error::Conflict), Ok(_)) => request_a,
        other => panic!("write skew or wrong outcomes: {other:?}"),
    };
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(store.receipt(&context, rejected).await.unwrap().is_none());
    let snapshot = snapshot(&mut store, &registry, &context).await;
    assert_eq!(snapshot.result.as_array().unwrap().len(), 3);
    assert_eq!(snapshot.revision.sequence, 2);
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn default_mutations_retry_three_commit_failures_with_the_original_request() {
    let (mut store, mut registry, context) = fixture().await;
    let namespace = store.schema().namespace().to_owned();
    admin(&format!("CREATE SEQUENCE \"{namespace}\".fault_attempt; CREATE FUNCTION \"{namespace}\".retry_commit() RETURNS trigger LANGUAGE plpgsql AS $$ DECLARE n bigint; BEGIN n := nextval('\"{namespace}\".fault_attempt'); IF n = 1 THEN RAISE EXCEPTION 'serialization fault' USING ERRCODE='40001'; ELSIF n = 2 THEN RAISE EXCEPTION 'deadlock fault' USING ERRCODE='40P01'; ELSIF n = 3 THEN RAISE EXCEPTION 'serialization fault' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER retry_commit AFTER INSERT ON \"{namespace}\".receipts DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION \"{namespace}\".retry_commit();")).await;
    let calls = Arc::new(AtomicUsize::new(0));
    registry
        .mutation("retry_bundle", CountedMutation(calls.clone()), Policy)
        .unwrap();
    let request = Uuid::now_v7();
    let receipt = store
        .mutate(&registry, &context, request, "retry_bundle", bundle_input())
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert_eq!(receipt.request_id, request);
    assert_eq!(receipt.revision.sequence, 1);
    assert_eq!(
        snapshot(&mut store, &registry, &context)
            .await
            .result
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        store
            .mutate(&registry, &context, request, "retry_bundle", bundle_input())
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn serializable_retry_exhaustion_rolls_back_and_business_conflicts_do_not_retry() {
    let (mut store, mut registry, context) = fixture().await;
    let namespace = store.schema().namespace().to_owned();
    admin(&format!("CREATE FUNCTION \"{namespace}\".always_retry() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'serialization fault' USING ERRCODE='40001'; END $$; CREATE CONSTRAINT TRIGGER always_retry AFTER INSERT ON \"{namespace}\".receipts DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION \"{namespace}\".always_retry();")).await;
    let calls = Arc::new(AtomicUsize::new(0));
    registry
        .mutation_with_concurrency(
            "bounded",
            CountedMutation(calls.clone()),
            Policy,
            serializable(2),
        )
        .unwrap();
    let request = Uuid::now_v7();
    assert!(matches!(
        store
            .mutate(&registry, &context, request, "bounded", bundle_input())
            .await,
        Err(Error::RetryExhausted)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(store.receipt(&context, request).await.unwrap().is_none());
    assert_eq!(
        snapshot(&mut store, &registry, &context)
            .await
            .revision
            .sequence,
        0
    );
    registry
        .mutation("default_bounded", CountedMutation(calls.clone()), Policy)
        .unwrap();
    let default_request = Uuid::now_v7();
    assert!(matches!(
        store
            .mutate(
                &registry,
                &context,
                default_request,
                "default_bounded",
                bundle_input()
            )
            .await,
        Err(Error::RetryExhausted)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    assert!(
        store
            .receipt(&context, default_request)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        snapshot(&mut store, &registry, &context).await.result,
        json!([])
    );
    admin(&format!("DROP TRIGGER always_retry ON \"{namespace}\".receipts; DROP FUNCTION \"{namespace}\".always_retry();")).await;
    assert!(matches!(
        store
            .mutate(
                &registry,
                &context,
                request,
                "bounded",
                json!({"publication":"daily","title":"Same","second":"Same"})
            )
            .await,
        Err(Error::Conflict)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 7);
    assert_eq!(
        snapshot(&mut store, &registry, &context).await.result,
        json!([])
    );
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn serializable_duplicate_races_recover_without_replaying_a_conflicting_handler() {
    let (mut store, mut registry, context) = fixture().await;
    let mut other = ManagedStore::connect(&url(), store.schema().clone())
        .await
        .unwrap();
    other.verify().await.unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    registry
        .mutation_with_concurrency(
            "duplicate",
            OverlappingMutation {
                barrier: Arc::new(tokio::sync::Barrier::new(2)),
                calls: calls.clone(),
                enforce_limit: false,
            },
            Policy,
            serializable(1),
        )
        .unwrap();
    let request = Uuid::now_v7();
    let (a, b) = tokio::join!(
        store.mutate(&registry, &context, request, "duplicate", json!({})),
        other.mutate(&registry, &context, request, "duplicate", json!({})),
    );
    assert_eq!(a.unwrap(), b.unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        snapshot(&mut store, &registry, &context)
            .await
            .revision
            .sequence,
        1
    );
    assert!(matches!(
        store
            .mutate(
                &registry,
                &context,
                request,
                "duplicate",
                json!({"changed":true})
            )
            .await,
        Err(Error::RequestIdReused)
    ));
    cleanup(&store).await;
}

fn slow_mutation<'a>(
    context: MutationContext<'a, Identity>,
    _: &'a Json,
) -> FunctionFuture<'a, Json> {
    Box::pin(async move {
        context.db.insert("markers", RecordValues::new()).await?;
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        Ok(json!({"inserted":true}))
    })
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn serializable_deadlines_distinguish_precommit_rollback_from_unknown_commit_outcomes() {
    use hulladb_postgres::managed::{MutationConcurrency, RetryOptions};
    let (mut store, mut registry, context) = fixture().await;
    let policy = MutationConcurrency::Serializable(RetryOptions {
        timeout: std::time::Duration::from_millis(100),
        ..RetryOptions::default()
    });
    registry
        .mutation_with_concurrency(
            "slow",
            MutationFn {
                call: slow_mutation,
                invalid_result: false,
            },
            Policy,
            policy,
        )
        .unwrap();
    let request = Uuid::now_v7();
    assert!(matches!(
        store
            .mutate(&registry, &context, request, "slow", json!({}))
            .await,
        Err(Error::TransactionDeadline)
    ));
    assert!(store.receipt(&context, request).await.unwrap().is_none());
    let namespace = store.schema().namespace().to_owned();
    admin(&format!("CREATE FUNCTION \"{namespace}\".slow_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(0.4); RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER slow_commit AFTER INSERT ON \"{namespace}\".receipts DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION \"{namespace}\".slow_commit();")).await;
    let calls = Arc::new(AtomicUsize::new(0));
    registry
        .mutation_with_concurrency("ambiguous", CountedMutation(calls.clone()), Policy, policy)
        .unwrap();
    let request = Uuid::now_v7();
    assert!(matches!(
        store
            .mutate(&registry, &context, request, "ambiguous", bundle_input())
            .await,
        Err(Error::CommitOutcomeUnknown)
    ));
    let receipt = store.receipt(&context, request).await.unwrap().unwrap();
    assert_eq!(receipt.revision.sequence, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn serializable_retries_share_one_execution_deadline() {
    use hulladb_postgres::managed::{MutationConcurrency, RetryOptions};
    let (mut store, mut registry, context) = fixture().await;
    let namespace = store.schema().namespace().to_owned();
    // Fail before COMMIT so the second slow handler must use the original budget.
    admin(&format!("CREATE SEQUENCE \"{namespace}\".fault_attempt; CREATE FUNCTION \"{namespace}\".retry_once() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('\"{namespace}\".fault_attempt') = 1 THEN RAISE EXCEPTION 'serialization fault' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_once BEFORE INSERT ON \"{namespace}\".receipts FOR EACH ROW EXECUTE FUNCTION \"{namespace}\".retry_once();")).await;
    registry
        .mutation_with_concurrency(
            "slow_retry",
            MutationFn {
                call: slow_mutation,
                invalid_result: false,
            },
            Policy,
            MutationConcurrency::Serializable(RetryOptions {
                max_attempts: 4,
                timeout: std::time::Duration::from_millis(1500),
                backoff: std::time::Duration::from_millis(1),
            }),
        )
        .unwrap();
    let request = Uuid::now_v7();
    assert!(matches!(
        store
            .mutate(&registry, &context, request, "slow_retry", json!({}))
            .await,
        Err(Error::TransactionDeadline)
    ));
    assert!(store.receipt(&context, request).await.unwrap().is_none());
    assert_eq!(store.managed_head(&context).await.unwrap().1.sequence, 0);
    // A retry succeeded under a restarted deadline would advance the sequence.
    cleanup(&store).await;
}

struct PausedAuthorizedMutation {
    entered: Arc<tokio::sync::Notify>,
    resume: Arc<tokio::sync::Notify>,
    calls: Arc<AtomicUsize>,
}
impl FunctionContract for PausedAuthorizedMutation {
    fn validate_input(&self, input: &Json) -> Result<(), Error> {
        Articles.validate_input(input)
    }
    fn validate_result(&self, result: &Json) -> Result<(), Error> {
        if result.is_object() {
            Ok(())
        } else {
            Err(Error::CorruptData)
        }
    }
}
impl Mutation<Identity> for PausedAuthorizedMutation {
    fn call<'a>(
        &'a self,
        context: MutationContext<'a, Identity>,
        _: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            context.db.insert("markers", RecordValues::new()).await?;
            self.entered.notify_one();
            self.resume.notified().await;
            Ok(json!({"inserted": true}))
        })
    }
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn serializable_retries_recheck_revoked_permissions_before_replaying_the_handler() {
    let (mut store, mut registry, admin_context) = fixture().await;
    store
        .mutate(
            &registry,
            &admin_context,
            Uuid::now_v7(),
            "permission",
            json!({"id":"reader"}),
        )
        .await
        .unwrap();
    let user = Context {
        principal: Uuid::now_v7(),
        authorization_scope: Uuid::now_v7(),
        identity: Identity {
            admin: false,
            owner: "reader".into(),
        },
        ..admin_context.clone()
    };
    let mut other = ManagedStore::connect(&url(), store.schema().clone())
        .await
        .unwrap();
    other.verify().await.unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    registry
        .mutation_with_concurrency(
            "authorized",
            PausedAuthorizedMutation {
                entered: entered.clone(),
                resume: resume.clone(),
                calls: calls.clone(),
            },
            Policy,
            serializable(3),
        )
        .unwrap();
    let request = Uuid::now_v7();
    let (result, revocation) = tokio::join!(
        store.mutate(&registry, &user, request, "authorized", json!({})),
        async {
            entered.notified().await;
            let result = other
                .mutate(
                    &registry,
                    &admin_context,
                    Uuid::now_v7(),
                    "revoke",
                    json!({"id":"reader"}),
                )
                .await;
            resume.notify_one();
            result
        },
    );
    revocation.unwrap();
    assert!(matches!(result, Err(Error::Forbidden)), "{result:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(store.receipt(&user, request).await.unwrap().is_none());
    assert_eq!(
        snapshot(&mut store, &registry, &admin_context)
            .await
            .revision
            .sequence,
        2
    );
    cleanup(&store).await;
}

fn predicate_scenario<'a>(
    ctx: MutationContext<'a, Identity>,
    _: &'a Json,
) -> FunctionFuture<'a, Json> {
    Box::pin(async move {
        for (id, tag) in [("a", Some("100%_\\")), ("b", Some("other")), ("c", None)] {
            ctx.db
                .insert(
                    "permissions",
                    RecordValues::from([
                        ("id".into(), Value::String(id.into())),
                        ("allowed".into(), Value::Boolean(true)),
                        (
                            "tag".into(),
                            tag.map(|v| Value::String(v.into())).unwrap_or(Value::Null),
                        ),
                    ]),
                )
                .await?;
        }
        let filters =
            |v| serde_json::from_value::<Vec<hulladb_postgres::managed::Predicate>>(v).unwrap();
        let compare = |field, operator, value| json!({"kind":"compare","field":field,"operator":operator,"operand":{"kind":"value","value":value}});
        let order = serde_json::from_value::<Vec<hulladb_postgres::managed::Order>>(
            json!([{"field":"allowed","direction":"desc"}]),
        )
        .unwrap();
        let rows = ctx
            .db
            .read()
            .select_where("permissions", &filters(json!([])), None, &order)
            .await?;
        assert_eq!(
            rows.iter()
                .map(|r| r.values["id"].clone())
                .collect::<Vec<_>>(),
            vec![
                Value::String("c".into()),
                Value::String("b".into()),
                Value::String("a".into())
            ]
        );
        let rows = ctx
            .db
            .read()
            .select_where(
                "permissions",
                &filters(json!([{"kind":"not","predicate":compare("tag","=",json!("other"))}])),
                None,
                &[],
            )
            .await?;
        assert_eq!(rows.len(), 1); // NULL remains UNKNOWN under NOT.
        let rows = ctx
            .db
            .read()
            .select_where(
                "permissions",
                &filters(json!([compare("tag", "contains", json!("%_\\"))])),
                None,
                &[],
            )
            .await?;
        assert_eq!(rows.len(), 1);
        let rows = ctx
            .db
            .read()
            .select_where(
                "permissions",
                &filters(json!([compare("tag", "in", json!([null, "other"]))])),
                None,
                &[],
            )
            .await?;
        assert_eq!(rows.len(), 2);
        let rows = ctx.db.read().select_where("permissions", &filters(json!([{"kind":"or","predicates":[compare("id","=",json!("a")),{"kind":"and","predicates":[compare("id","=",json!("b")),{"kind":"constant","value":false}]}]}])), None, &[]).await?;
        assert_eq!(rows.len(), 1);
        let rows = ctx.db.read().select_where("permissions", &filters(json!([{"kind":"compare","field":"tag","operator":"!=","operand":{"kind":"field","field":"id"}}])), None, &[]).await?;
        assert_eq!(rows.len(), 2);
        let all = filters(json!([{"kind":"or","predicates":[{"kind":"constant","value":true}]}]));
        assert_eq!(
            ctx.db
                .update_where(
                    "permissions",
                    &all,
                    RecordValues::from([("allowed".into(), Value::Boolean(false))])
                )
                .await?
                .len(),
            3
        );
        assert_eq!(
            ctx.db
                .delete_where(
                    "permissions",
                    &filters(json!([compare("tag", "is", Json::Null)]))
                )
                .await?
                .len(),
            1
        );
        Ok(json!({}))
    })
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make db-test"]
async fn scalar_predicates_preserve_sql_null_grouping_escaping_and_order() {
    let (mut store, mut registry, context) = fixture().await;
    registry
        .mutation(
            "predicates",
            MutationFn {
                call: predicate_scenario,
                invalid_result: false,
            },
            Policy,
        )
        .unwrap();
    registry
        .mutation(
            "predicate_rollback",
            MutationFn {
                call: predicate_scenario,
                invalid_result: true,
            },
            Policy,
        )
        .unwrap();
    assert!(
        store
            .mutate(
                &registry,
                &context,
                Uuid::now_v7(),
                "predicate_rollback",
                json!({})
            )
            .await
            .is_err()
    );
    store
        .mutate(&registry, &context, Uuid::now_v7(), "predicates", json!({}))
        .await
        .unwrap();
    let mut other = context.clone();
    other.scope = Uuid::now_v7();
    store.create_scope(other.scope).await.unwrap();
    store
        .mutate(&registry, &other, Uuid::now_v7(), "predicates", json!({}))
        .await
        .unwrap();
}
