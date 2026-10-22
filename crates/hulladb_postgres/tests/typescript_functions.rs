#![cfg(feature = "typescript")]

use std::{path::PathBuf, sync::Arc, time::Duration};

use hulladb_postgres::managed::{
    Authorization, Context, Error, FunctionFuture, Invocation, ManagedSchema, ManagedStore,
    QueryContext, Registry,
    typescript::{TypescriptFunction, Worker},
};
use serde_json::{Value as Json, json};
use uuid::Uuid;

mod generated {
    include!("../../hulladb_server/tests/fixtures/deployed_schema.rs");
}
mod publishing {
    include!("../../hulladb_schema/tests/fixtures/articles_schema.rs");
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/db")
}
fn url() -> String {
    std::env::var("HULLADB_TEST_URL")
        .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into())
}
fn worker(timeout: Duration) -> Arc<Worker> {
    let executable = std::env::var_os("HULLADB_JS")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join("../../target/debug/hulladb-js"));
    assert!(
        executable.is_file(),
        "build hulladb-js before running this test"
    );
    let mut worker = Worker::new(
        executable,
        vec![
            root()
                .join("tests/generated/deployed-worker.js")
                .into_os_string(),
        ],
    );
    worker.timeout = timeout;
    Arc::new(worker)
}

struct Policy;
impl Authorization<String> for Policy {
    fn authorize<'a>(
        &'a self,
        ctx: QueryContext<'a, String>,
        _: Invocation,
        _: &'a Json,
    ) -> FunctionFuture<'a, ()> {
        Box::pin(async move {
            if ctx.identity == "alice" {
                Ok(())
            } else {
                Err(Error::Forbidden)
            }
        })
    }
}
struct DeniedPolicy;
impl Authorization<String> for DeniedPolicy {
    fn authorize<'a>(
        &'a self,
        _: QueryContext<'a, String>,
        _: Invocation,
        _: &'a Json,
    ) -> FunctionFuture<'a, ()> {
        Box::pin(async { Err(Error::Forbidden) })
    }
}
fn registry(worker: Arc<Worker>) -> Registry<String> {
    let manifest = std::fs::read_to_string(root().join("tests/generated/functions.json"))
        .expect("run make sdk-check first");
    let mut registry = Registry::new();
    for name in [
        "deployed.create",
        "deployed.list",
        "deployed.fail",
        "typescript.edit",
        "typescript.crash",
        "typescript.hang",
        "typescript.invalid",
        "typescript.nonUnique",
        "typescript.leaked",
        "typescript.queryWrite",
        "typescript.environment",
        "typescript.parallel",
    ] {
        TypescriptFunction::from_manifest(worker.clone(), name, &manifest)
            .unwrap()
            .register(&mut registry, Policy)
            .unwrap();
    }
    registry
}
async fn fixture() -> (ManagedStore, Context<String>) {
    let schema = ManagedSchema::new(
        format!("ts_{}", Uuid::now_v7().simple()),
        generated::schema().unwrap(),
    )
    .unwrap();
    let mut store = ManagedStore::connect(&url(), schema).await.unwrap();
    store.initialize().await.unwrap();
    let scope = Uuid::now_v7();
    store.create_scope(scope).await.unwrap();
    (
        store,
        Context {
            scope,
            principal: Uuid::now_v7(),
            authorization_scope: Uuid::now_v7(),
            identity: "alice".into(),
        },
    )
}
async fn cleanup(store: &ManagedStore) {
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!(
            "DROP SCHEMA \"{}\" CASCADE",
            store.schema().namespace()
        ))
        .await
        .unwrap();
    drop(client);
    connection.await.unwrap().unwrap();
}

#[tokio::test]
#[ignore = "requires PostgreSQL and hulladb-js; run make typescript-functions-test"]
async fn typescript_crud_and_receipts_share_the_authorized_native_transaction() {
    let (mut store, ctx) = fixture().await;
    let worker = worker(Duration::from_secs(3));
    let registry = registry(worker.clone());
    assert!(std::env::var_os("PATH").is_some());
    assert_eq!(
        store
            .query(&registry, &ctx, "typescript.environment", Json::Null)
            .await
            .unwrap()
            .result,
        "absent"
    );
    let request = Uuid::now_v7();
    let input = json!({"title": "kept", "ordinal": i64::MAX.to_string()});
    let receipt = store
        .mutate(&registry, &ctx, request, "deployed.create", input.clone())
        .await
        .unwrap();
    assert_eq!(receipt.result["ordinal"], i64::MAX.to_string());
    assert_eq!(receipt.result["owner"], "alice");
    let parallel = store
        .query(&registry, &ctx, "typescript.parallel", Json::Null)
        .await
        .unwrap();
    assert_eq!(
        parallel.result,
        json!({"first": [receipt.result], "second": [receipt.result]})
    );
    // A second execution would conflict on the unique owner/title constraint.
    assert_eq!(
        store
            .mutate(&registry, &ctx, request, "deployed.create", input.clone())
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(
        store.receipt(&ctx, request).await.unwrap(),
        Some(receipt.clone())
    );
    let manifest = std::fs::read_to_string(root().join("tests/generated/functions.json")).unwrap();
    let mut denied_registry = Registry::new();
    for name in ["deployed.create", "deployed.list"] {
        TypescriptFunction::from_manifest(worker.clone(), name, &manifest)
            .unwrap()
            .register(&mut denied_registry, DeniedPolicy)
            .unwrap();
    }
    // Application policy changes do not revoke an already committed response.
    // The trusted identity and authorization scope remain the original ones.
    assert_eq!(
        store.receipt(&ctx, request).await.unwrap(),
        Some(receipt.clone())
    );
    assert_eq!(
        store
            .mutate(
                &denied_registry,
                &ctx,
                request,
                "deployed.create",
                input.clone()
            )
            .await
            .unwrap(),
        receipt
    );
    assert!(matches!(
        store
            .mutate(
                &denied_registry,
                &ctx,
                Uuid::now_v7(),
                "deployed.create",
                input
            )
            .await,
        Err(Error::Forbidden)
    ));
    // Verified identity fences still prevent delivering the saved response.
    let mut changed_claims = ctx.clone();
    changed_claims.authorization_scope = Uuid::now_v7();
    assert!(matches!(
        store.receipt(&changed_claims, request).await,
        Err(Error::Forbidden)
    ));
    let mut other_principal = ctx.clone();
    other_principal.principal = Uuid::now_v7();
    assert!(
        store
            .receipt(&other_principal, request)
            .await
            .unwrap()
            .is_none()
    );
    let result = store
        .mutate(
            &registry,
            &ctx,
            Uuid::now_v7(),
            "typescript.edit",
            json!({"title": "temporary"}),
        )
        .await
        .unwrap();
    assert_eq!(result.result["inserted"]["ordinal"], "0");
    assert_eq!(result.result["updated"]["ordinal"], i64::MAX.to_string());
    assert_eq!(result.result["absent"], true);
    let snapshot = store
        .query(&registry, &ctx, "deployed.list", Json::Null)
        .await
        .unwrap();
    assert_eq!(snapshot.result.as_array().unwrap().len(), 1);
    assert_eq!(snapshot.tables.into_iter().collect::<Vec<_>>(), ["entries"]);
    assert!(matches!(
        store
            .query(&denied_registry, &ctx, "deployed.list", Json::Null)
            .await,
        Err(Error::Forbidden)
    ));
    store
        .set_scope_quota(
            ctx.scope,
            hulladb_postgres::managed::ScopeQuota {
                max_rows: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let head = store.managed_head(&ctx).await.unwrap();
    let request = Uuid::now_v7();
    assert!(matches!(
        store
            .mutate(
                &registry,
                &ctx,
                request,
                "deployed.create",
                json!({"title": "over capacity", "ordinal": "1"})
            )
            .await,
        Err(Error::QuotaExceeded)
    ));
    assert_eq!(store.managed_head(&ctx).await.unwrap(), head);
    assert!(store.receipt(&ctx, request).await.unwrap().is_none());
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL and hulladb-js; run make typescript-functions-test"]
async fn worker_failures_and_cancellation_leave_no_rows_revision_or_receipts() {
    let (mut store, ctx) = fixture().await;
    let registry = registry(worker(Duration::from_secs(1)));
    let initial = store.managed_head(&ctx).await.unwrap();
    for name in [
        "deployed.fail",
        "typescript.crash",
        "typescript.hang",
        "typescript.invalid",
        "typescript.nonUnique",
        "typescript.leaked",
    ] {
        let request = Uuid::now_v7();
        let input = if name == "deployed.fail" {
            json!({"title": name, "ordinal": "0"})
        } else {
            json!({"title": name})
        };
        let error = store
            .mutate(&registry, &ctx, request, name, input)
            .await
            .unwrap_err();
        match name {
            "deployed.fail" => {
                assert!(matches!(error, Error::Conflict), "{name}: {error:?}")
            }
            "typescript.nonUnique" => assert!(
                matches!(error, Error::Application(ref error) if error.code == "not_unique"),
                "{error:?}"
            ),
            "typescript.hang" => assert!(matches!(error, Error::TransactionDeadline), "{error:?}"),
            _ => assert!(
                matches!(error, Error::FunctionExecution | Error::InvalidInput(_)),
                "{name}: {error:?}"
            ),
        }
        assert!(store.receipt(&ctx, request).await.unwrap().is_none());
        assert_eq!(store.managed_head(&ctx).await.unwrap(), initial);
    }
    let request = Uuid::now_v7();
    let cancelled = tokio::time::timeout(
        Duration::from_millis(300),
        store.mutate(
            &registry,
            &ctx,
            request,
            "typescript.hang",
            json!({"title": "cancelled"}),
        ),
    )
    .await;
    assert!(cancelled.is_err());
    assert!(store.receipt(&ctx, request).await.unwrap().is_none());
    assert_eq!(store.managed_head(&ctx).await.unwrap(), initial);
    assert!(matches!(
        store
            .query(&registry, &ctx, "typescript.queryWrite", Json::Null)
            .await,
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(
        store
            .query(&registry, &ctx, "deployed.list", Json::Null)
            .await
            .unwrap()
            .result,
        json!([])
    );
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL and hulladb-js; run make typescript-functions-test"]
async fn changed_schema_and_bounded_worker_channels_cannot_commit() {
    let (mut store, ctx) = fixture().await;
    let manifest = std::fs::read_to_string(root().join("tests/generated/functions.json")).unwrap();
    // The full guard must also accept valid references, custom key metadata,
    // nullable fields, unique null policies and extreme/literal defaults.
    let schema = ManagedSchema::new(
        format!("ts_{}", Uuid::now_v7().simple()),
        publishing::schema().unwrap(),
    )
    .unwrap();
    let mut publishing_store = ManagedStore::connect(&url(), schema).await.unwrap();
    publishing_store.initialize().await.unwrap();
    publishing_store.create_scope(ctx.scope).await.unwrap();
    let mut publishing_worker = (*worker(Duration::from_secs(2))).clone();
    publishing_worker.arguments = vec![
        root()
            .join("tests/generated/schema-worker.js")
            .into_os_string(),
    ];
    let mut publishing_registry = Registry::new();
    TypescriptFunction::from_manifest(
        Arc::new(publishing_worker),
        "typescript.environment",
        &manifest,
    )
    .unwrap()
    .register(&mut publishing_registry, Policy)
    .unwrap();
    publishing_store
        .query(
            &publishing_registry,
            &ctx,
            "typescript.environment",
            Json::Null,
        )
        .await
        .unwrap();
    cleanup(&publishing_store).await;
    for mode in [
        "write",
        "enqueue",
        "oversized",
        "sequence",
        "invalid-result",
    ] {
        let configured = worker(Duration::from_secs(1));
        let mut configured = (*configured).clone();
        // These deliberately invalid peers test the native protocol boundary,
        // independently of the restricted JavaScript runtime.
        let frame = match mode {
            "write" => json!({"type": "operation", "id": 1, "operation": {
                "method": "insert", "table": "entries",
                "values": {"title": "forbidden", "owner": "alice"}
            }})
            .to_string(),
            "enqueue" => json!({"type": "operation", "id": 1, "operation": {
                "method": "enqueue", "name": "entries.created", "payload": {},
                "max_attempts": 2, "delay_ms": 0
            }})
            .to_string(),
            "oversized" => "x".repeat(8192),
            "sequence" => json!({"type": "operation", "id": 2, "operation": {
                "method": "select", "table": "entries", "filters": [], "limit": null
            }})
            .to_string(),
            "invalid-result" => json!({"type": "result", "result": 1}).to_string(),
            _ => unreachable!(),
        };
        configured.executable = "/bin/sh".into();
        configured.arguments = vec![
            "-c".into(),
            // Pass the frame as an argument, never interpolate it into shell code.
            "IFS= read -r invocation; printf '%s\n' \"$1\"".into(),
            "protocol-peer".into(),
            frame.into(),
        ];
        configured.max_frame_bytes = 4096;
        let mut registry = Registry::new();
        TypescriptFunction::from_manifest(Arc::new(configured), "deployed.list", &manifest)
            .unwrap()
            .register(&mut registry, Policy)
            .unwrap();
        let error = store
            .query(&registry, &ctx, "deployed.list", Json::Null)
            .await
            .unwrap_err();
        match mode {
            "write" => assert!(matches!(error, Error::Forbidden), "{error:?}"),
            "invalid-result" => assert!(matches!(error, Error::CorruptData), "{error:?}"),
            _ => assert!(matches!(error, Error::FunctionExecution), "{error:?}"),
        }
    }
    let mut limited = (*worker(Duration::from_secs(2))).clone();
    limited.max_operations = 1;
    let limited_registry = registry(Arc::new(limited));
    let head = store.managed_head(&ctx).await.unwrap();
    let request = Uuid::now_v7();
    assert!(matches!(
        store
            .mutate(
                &limited_registry,
                &ctx,
                request,
                "typescript.edit",
                json!({"title": "budget rollback"})
            )
            .await,
        Err(Error::TransactionDeadline)
    ));
    assert_eq!(store.managed_head(&ctx).await.unwrap(), head);
    assert!(store.receipt(&ctx, request).await.unwrap().is_none());
    let mut changed = generated::schema().unwrap().tables()[0].fields().to_vec();
    changed
        .iter_mut()
        .find(|field| field.name == "ordinal")
        .unwrap()
        .default = Some(hulladb_schema::Value::Int64(99));
    let mut table = hulladb_schema::TableDeclaration::new("entries", changed);
    table
        .unique_constraints
        .push(hulladb_schema::UniqueConstraintDeclaration::new(
            "owner_title",
            vec!["owner".into(), "title".into()],
        ));
    let schema = ManagedSchema::new(
        format!("ts_{}", Uuid::now_v7().simple()),
        hulladb_schema::SchemaDeclaration::new(vec![table])
            .normalize()
            .unwrap(),
    )
    .unwrap();
    let mut changed_store = ManagedStore::connect(&url(), schema).await.unwrap();
    changed_store.initialize().await.unwrap();
    changed_store.create_scope(ctx.scope).await.unwrap();
    let registry = registry(worker(Duration::from_secs(2)));
    assert!(matches!(
        changed_store
            .query(&registry, &ctx, "deployed.list", Json::Null)
            .await,
        Err(Error::SchemaMismatch)
    ));
    cleanup(&changed_store).await;
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL and hulladb-js; run make typescript-functions-test"]
async fn engine_deadline_is_reported_without_committing() {
    let (mut store, ctx) = fixture().await;
    // Keep the parent deadline beyond the engine's own five-second budget so
    // this exercises the execution_limit frame rather than Tokio cancellation.
    let registry = registry(worker(Duration::from_secs(8)));
    let initial = store.managed_head(&ctx).await.unwrap();
    let request = Uuid::now_v7();
    let error = store
        .mutate(
            &registry,
            &ctx,
            request,
            "typescript.hang",
            json!({"title": "engine deadline rollback"}),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, Error::TransactionDeadline), "{error:?}");
    assert_eq!(store.managed_head(&ctx).await.unwrap(), initial);
    assert!(store.receipt(&ctx, request).await.unwrap().is_none());
    assert_eq!(
        store
            .query(&registry, &ctx, "deployed.list", Json::Null)
            .await
            .unwrap()
            .result,
        json!([])
    );
    cleanup(&store).await;
}
