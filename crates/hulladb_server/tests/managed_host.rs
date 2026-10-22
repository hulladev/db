use axum::{Router, routing::post};
use hulladb_postgres::managed::{
    Authorization, Context, Error, FunctionContract, FunctionFuture, Invocation, ManagedSchema,
    ManagedStore, Mutation, MutationContext, Query, QueryContext, Registry, Select,
};
use hulladb_schema::{RecordValues, Value};
use hulladb_server::{
    AuthenticationError, HostOptions,
    managed::{AuthenticationFuture, Authenticator, CredentialHeaders, ManagedHost, Options},
};
use serde_json::{Value as Json, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;
mod generated {
    include!("fixtures/deployed_schema.rs");
}

struct Credentials(Mutex<HashMap<String, Context<String>>>);
impl Authenticator<String> for Credentials {
    fn authenticate<'a>(
        &'a self,
        headers: &'a CredentialHeaders,
    ) -> AuthenticationFuture<'a, String> {
        Box::pin(async move {
            let mut values = headers.get_all("authorization").iter();
            let value = values.next().and_then(|value| value.to_str().ok());
            if values.next().is_some() {
                return Err(AuthenticationError::InvalidCredentials);
            }
            let (_, bearer) = value
                .and_then(|value| value.split_once(' '))
                .filter(|(scheme, bearer)| {
                    scheme.eq_ignore_ascii_case("Bearer")
                        && !bearer.is_empty()
                        && !bearer.bytes().any(|byte| byte.is_ascii_whitespace())
                })
                .ok_or(AuthenticationError::InvalidCredentials)?;

            self.0
                .lock()
                .unwrap()
                .get(bearer)
                .cloned()
                .ok_or(AuthenticationError::InvalidCredentials)
        })
    }
}
struct Policy;
impl Authorization<String> for Policy {
    fn authorize<'a>(
        &'a self,
        _: QueryContext<'a, String>,
        _: Invocation,
        _: &'a Json,
    ) -> FunctionFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}
fn row_json(row: &hulladb_postgres::managed::Row) -> Json {
    let Value::Uuid(id) = row.values["id"] else {
        panic!("invalid ID")
    };
    let Value::String(title) = &row.values["title"] else {
        panic!("invalid title")
    };
    let Value::String(owner) = &row.values["owner"] else {
        panic!("invalid owner")
    };
    let Value::Int64(ordinal) = row.values["ordinal"] else {
        panic!("invalid int64")
    };
    json!({"id": Uuid::from_bytes(id), "title": title, "owner": owner, "ordinal": ordinal.to_string()})
}
struct List;
impl FunctionContract for List {
    fn validate_input(&self, input: &Json) -> Result<(), Error> {
        if input.is_null() {
            Ok(())
        } else {
            Err(Error::InvalidInput("no arguments".into()))
        }
    }
    fn validate_result(&self, result: &Json) -> Result<(), Error> {
        if result.is_array() {
            Ok(())
        } else {
            Err(Error::CorruptData)
        }
    }
}
impl Query<String> for List {
    fn call<'a>(
        &'a self,
        context: QueryContext<'a, String>,
        _: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        Box::pin(async move {
            let rows = context
                .db
                .select(
                    "entries",
                    Select {
                        equals: RecordValues::from([(
                            "owner".into(),
                            Value::String(context.identity.clone()),
                        )]),
                        ..Select::default()
                    },
                )
                .await?;
            Ok(Json::Array(rows.iter().map(row_json).collect()))
        })
    }
}
struct Create {
    fail: bool,
}
impl FunctionContract for Create {
    fn validate_input(&self, input: &Json) -> Result<(), Error> {
        if input.as_object().is_some_and(|o| o.len() == 2)
            && input["title"].as_str().is_some()
            && input["ordinal"]
                .as_str()
                .and_then(|v| v.parse::<i64>().ok())
                .is_some()
        {
            Ok(())
        } else {
            Err(Error::InvalidInput("invalid generated input".into()))
        }
    }
    fn validate_result(&self, result: &Json) -> Result<(), Error> {
        if result.is_object() {
            Ok(())
        } else {
            Err(Error::CorruptData)
        }
    }
}
impl Mutation<String> for Create {
    fn call<'a>(
        &'a self,
        context: MutationContext<'a, String>,
        input: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        Box::pin(async move {
            let values = RecordValues::from([
                (
                    "title".into(),
                    Value::String(input["title"].as_str().unwrap().into()),
                ),
                ("owner".into(), Value::String(context.identity.clone())),
                (
                    "ordinal".into(),
                    Value::Int64(input["ordinal"].as_str().unwrap().parse().unwrap()),
                ),
            ]);
            let row = context.db.insert("entries", values.clone()).await?;
            if self.fail {
                context.db.insert("entries", values).await?;
            }
            Ok(row_json(&row))
        })
    }
}
fn url() -> String {
    std::env::var("HULLADB_TEST_URL")
        .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into())
}

#[tokio::test]
#[ignore = "requires PostgreSQL and Bun; run make sdk-adapters-test"]
async fn generated_sdk_executes_generic_postgres_functions_and_manages_http_sse_lifecycles() {
    sdk_lifecycle(false).await;
}

#[cfg(feature = "typescript")]
#[tokio::test]
#[ignore = "requires PostgreSQL, hulladb-js and Bun; run make typescript-functions-test"]
async fn generated_sdk_executes_typescript_callbacks_with_http_sse_and_receipt_recovery() {
    sdk_lifecycle(true).await;
}

async fn sdk_lifecycle(typescript: bool) {
    let schema = ManagedSchema::new(
        format!("sdk_{}", Uuid::now_v7().simple()),
        generated::schema().unwrap(),
    )
    .unwrap();
    let mut store = ManagedStore::connect(&url(), schema.clone()).await.unwrap();
    store.initialize().await.unwrap();
    let scope = Uuid::now_v7();
    store.create_scope(scope).await.unwrap();
    let alice = Context {
        scope,
        principal: Uuid::now_v7(),
        authorization_scope: Uuid::now_v7(),
        identity: "alice".into(),
    };
    let bob = Context {
        scope,
        principal: Uuid::now_v7(),
        authorization_scope: Uuid::now_v7(),
        identity: "bob".into(),
    };
    let credentials = Arc::new(Credentials(Mutex::new(HashMap::from([
        ("alice".into(), alice.clone()),
        ("bob".into(), bob.clone()),
    ]))));
    let mut registry = Registry::new();
    registry.query("deployed.list", List, Policy).unwrap();
    registry
        .mutation_with_concurrency(
            "deployed.create",
            Create { fail: false },
            Policy,
            hulladb_postgres::managed::MutationConcurrency::Serializable(
                hulladb_postgres::managed::RetryOptions {
                    max_attempts: 3,
                    timeout: Duration::from_secs(1),
                    backoff: Duration::from_millis(1),
                },
            ),
        )
        .unwrap();
    registry
        .mutation("deployed.fail", Create { fail: true }, Policy)
        .unwrap();
    #[cfg(feature = "typescript")]
    if typescript {
        use hulladb_postgres::managed::typescript::{TypescriptFunction, Worker};
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/db");
        let executable = std::env::var_os("HULLADB_JS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| root.join("../../target/debug/hulladb-js"));
        assert!(
            executable.is_file(),
            "build hulladb-js before running this test"
        );
        let worker = Arc::new(Worker::new(
            executable,
            vec![
                root.join("tests/generated/deployed-worker.js")
                    .into_os_string(),
            ],
        ));
        let manifest =
            std::fs::read_to_string(root.join("tests/generated/functions.json")).unwrap();
        registry = Registry::new();
        for name in ["deployed.list", "deployed.create", "deployed.fail"] {
            TypescriptFunction::from_manifest(worker.clone(), name, &manifest)
                .unwrap()
                .register_with_concurrency(
                    &mut registry,
                    Policy,
                    hulladb_postgres::managed::MutationConcurrency::Serializable(
                        hulladb_postgres::managed::RetryOptions {
                            max_attempts: 3,
                            timeout: Duration::from_secs(1),
                            backoff: Duration::from_millis(1),
                        },
                    ),
                )
                .unwrap();
        }
    }
    #[cfg(not(feature = "typescript"))]
    assert!(!typescript);
    let host = ManagedHost::connect(
        &url(),
        schema.clone(),
        registry,
        credentials.clone(),
        Options {
            budgets: HostOptions {
                poll_interval: Duration::from_millis(20),
                credential_check_interval: Duration::from_millis(20),
                operation_timeout: Duration::from_secs(2),
                database_connections: 2,
                max_inflight_requests: 16,
                max_subscriptions: 4,
                max_body_bytes: 65536,
            },
            cors_origins: vec!["https://sdk.example.test".into()],
            principal_quotas: None,
        },
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let router: Router = host.router().route(
        "/fixture/revoke",
        post(move || {
            let credentials = credentials.clone();
            async move {
                credentials.0.lock().unwrap().remove("bob");
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let settings = json!({"baseUrl": base_url, "alice": {"scope": scope, "principal": alice.principal, "authorizationScope": alice.authorization_scope, "bearer": "alice"}, "bob": {"scope": scope, "principal": bob.principal, "authorizationScope": bob.authorization_scope, "bearer": "bob"}});
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new("bun")
            .args([
                "--conditions=hulladb-source",
                "tests/deployed.integration.ts",
            ])
            .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages/db"))
            .env("HULLADB_SDK_FIXTURE", settings.to_string())
            .output()
            .expect("Bun is required")
    })
    .await
    .unwrap();
    host.stop();
    server.abort();
    let (admin, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    admin
        .batch_execute(&format!("DROP SCHEMA \"{}\" CASCADE", schema.namespace()))
        .await
        .unwrap();
    connection.abort();
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
#[ignore = "requires PostgreSQL; run make production-test"]
async fn host_admission_isolates_principals_and_a_cancelled_probe_does_not_poison_the_pool() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use hulladb_server::managed::PrincipalQuotas;
    use tower::ServiceExt;
    let schema = ManagedSchema::new(
        format!("admission_{}", Uuid::now_v7().simple()),
        generated::schema().unwrap(),
    )
    .unwrap();
    let mut store = ManagedStore::connect(&url(), schema.clone()).await.unwrap();
    store.initialize().await.unwrap();
    let scope = Uuid::now_v7();
    store.create_scope(scope).await.unwrap();
    let alice = Context {
        scope,
        principal: Uuid::now_v7(),
        authorization_scope: Uuid::now_v7(),
        identity: "alice".to_owned(),
    };
    let bob = Context {
        principal: Uuid::now_v7(),
        identity: "bob".into(),
        ..alice.clone()
    };
    let credentials = Arc::new(Credentials(Mutex::new(HashMap::from([
        ("alice".into(), alice.clone()),
        ("bob".into(), bob),
    ]))));
    let mut registry = Registry::new();
    registry.query("deployed.list", List, Policy).unwrap();
    let host = ManagedHost::connect(
        &url(),
        schema.clone(),
        registry,
        credentials,
        Options {
            budgets: HostOptions {
                database_connections: 1,
                operation_timeout: Duration::from_millis(100),
                poll_interval: Duration::from_millis(20),
                credential_check_interval: Duration::from_secs(1),
                max_inflight_requests: 4,
                max_subscriptions: 2,
                max_body_bytes: 65536,
            },
            cors_origins: vec![],
            principal_quotas: Some(PrincipalQuotas {
                requests_per_minute: 2,
                max_inflight: 1,
                max_subscriptions: 1,
                max_identities: 2,
            }),
        },
    )
    .await
    .unwrap();
    assert!(host.ready().await);
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!(
            "BEGIN; LOCK TABLE \"{}\".metadata IN ACCESS EXCLUSIVE MODE",
            schema.namespace()
        ))
        .await
        .unwrap();
    assert!(!host.ready().await);
    client.batch_execute("ROLLBACK").await.unwrap();
    let (source, _) = store.managed_head(&alice).await.unwrap();
    let router = host.router();
    let query = Request::builder()
        .method("POST")
        .uri("/functions/query")
        .header("Authorization", "Bearer alice")
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({"name":"deployed.list","input":null,"expected_source":source}).to_string(),
        ))
        .unwrap();
    assert_eq!(
        router.clone().oneshot(query).await.unwrap().status(),
        StatusCode::OK
    );
    for (token, expected) in [
        ("alice", StatusCode::OK),
        ("alice", StatusCode::TOO_MANY_REQUESTS),
        ("bob", StatusCode::OK),
    ] {
        let request = Request::builder()
            .uri("/functions/session")
            .header("Authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            router.clone().oneshot(request).await.unwrap().status(),
            expected
        );
    }
    let stats = host.stats();
    assert_eq!(stats.requests_started, 4);
    assert_eq!(stats.requests_completed, 4);
    assert_eq!(stats.successful_responses, 3);
    assert_eq!(stats.capacity_responses, 1);
    assert_eq!(stats.client_error_responses, 1);
    assert_eq!(stats.active_requests, 0);
    assert_eq!(stats.database_in_use, 0);
    assert_eq!(stats.database_waiters, 0);
    assert!(stats.request_duration_us > 0);
    // Authentication failures count even when admission never reaches a handler.
    let unauthorized = Request::builder()
        .uri("/functions/session")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        router.clone().oneshot(unauthorized).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(host.stats().client_error_responses, 2);
    host.stop();
    assert!(host.stats().stopped);
    assert!(!host.ready().await);
    client
        .batch_execute(&format!("DROP SCHEMA \"{}\" CASCADE", schema.namespace()))
        .await
        .unwrap();
    connection.abort();
}

struct CountedList(Arc<std::sync::atomic::AtomicUsize>);
impl FunctionContract for CountedList {
    fn validate_input(&self, input: &Json) -> Result<(), Error> {
        if input.is_object() {
            Ok(())
        } else {
            Err(Error::InvalidInput("object required".into()))
        }
    }
    fn validate_result(&self, result: &Json) -> Result<(), Error> {
        if result.is_object() {
            Ok(())
        } else {
            Err(Error::CorruptData)
        }
    }
}
impl Query<String> for CountedList {
    fn call<'a>(
        &'a self,
        context: QueryContext<'a, String>,
        input: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        Box::pin(async move {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let identity = context.identity.clone();
            let rows = List.call(context, &Json::Null).await?;
            if input["require_nonempty"] == true && rows.as_array().is_some_and(Vec::is_empty) {
                return Err(Error::NotFound);
            }
            Ok(json!({"identity":identity,"input":input,"rows":rows}))
        })
    }
}
struct LivePolicy {
    allowed: Arc<std::sync::atomic::AtomicBool>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}
impl Authorization<String> for LivePolicy {
    fn authorize<'a>(
        &'a self,
        _: QueryContext<'a, String>,
        _: Invocation,
        _: &'a Json,
    ) -> FunctionFuture<'a, ()> {
        Box::pin(async move {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.allowed.load(std::sync::atomic::Ordering::SeqCst) {
                Ok(())
            } else {
                Err(Error::Forbidden)
            }
        })
    }
}
struct ManagedEvents {
    body: axum::body::Body,
    buffer: Vec<u8>,
}
impl ManagedEvents {
    async fn next(&mut self) -> (String, Json) {
        use http_body_util::BodyExt;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(end) = self.buffer.windows(2).position(|b| b == b"\n\n") {
                    let frame = String::from_utf8(self.buffer.drain(..end + 2).collect()).unwrap();
                    let event = frame.lines().find_map(|s| s.strip_prefix("event: "));
                    let data = frame.lines().find_map(|s| s.strip_prefix("data: "));
                    if let (Some(event), Some(data)) = (event, data) {
                        return (event.into(), serde_json::from_str(data).unwrap());
                    }
                    continue;
                }
                let frame = self.body.frame().await.expect("stream ended").unwrap();
                if let Ok(bytes) = frame.into_data() {
                    self.buffer.extend_from_slice(&bytes);
                }
            }
        })
        .await
        .expect("subscription event deadline")
    }
    async fn snapshot(&mut self) -> Json {
        let (event, value) = self.next().await;
        assert_eq!(event, "round", "{value}");
        value
    }
}
async fn managed_watch(router: &Router, token: &str, source: Uuid, input: Json) -> ManagedEvents {
    let response = managed_subscribe(router, token, source, input).await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    ManagedEvents {
        body: response.into_body(),
        buffer: Vec::new(),
    }
}
async fn managed_subscribe(
    router: &Router,
    token: &str,
    source: Uuid,
    input: Json,
) -> axum::response::Response {
    use tower::ServiceExt;
    router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/functions/subscriptions")
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(
                    json!({"queries":[{"key":"list","name":"counted","input":input}],"expected_source":source}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn shared_subscriptions_coalesce_refreshes_preserve_identity_and_recheck_each_credential() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let schema = ManagedSchema::new(
        format!("subscriptions_{}", Uuid::now_v7().simple()),
        generated::schema().unwrap(),
    )
    .unwrap();
    let mut store = ManagedStore::connect(&url(), schema.clone()).await.unwrap();
    store.initialize().await.unwrap();
    let alice = Context {
        scope: Uuid::now_v7(),
        principal: Uuid::now_v7(),
        authorization_scope: Uuid::now_v7(),
        identity: "alice".to_owned(),
    };
    store.create_scope(alice.scope).await.unwrap();
    let bob = Context {
        principal: Uuid::now_v7(),
        identity: "bob".into(),
        ..alice.clone()
    };
    let claims = Context {
        authorization_scope: Uuid::now_v7(),
        ..alice.clone()
    };
    let other_scope = Context {
        scope: Uuid::now_v7(),
        ..alice.clone()
    };
    store.create_scope(other_scope.scope).await.unwrap();
    let credentials = Arc::new(Credentials(Mutex::new(HashMap::from([
        ("alice".into(), alice.clone()),
        ("alias".into(), alice.clone()),
        ("bob".into(), bob),
        ("claims".into(), claims),
        ("scope".into(), other_scope),
    ]))));
    let calls = Arc::new(AtomicUsize::new(0));
    let policy_calls = Arc::new(AtomicUsize::new(0));
    let allowed = Arc::new(AtomicBool::new(true));
    let mut registry = Registry::new();
    registry
        .query(
            "counted",
            CountedList(calls.clone()),
            LivePolicy {
                allowed: allowed.clone(),
                calls: policy_calls.clone(),
            },
        )
        .unwrap();
    let host = ManagedHost::connect(
        &url(),
        schema.clone(),
        registry,
        credentials.clone(),
        Options {
            budgets: HostOptions {
                poll_interval: Duration::from_millis(150),
                credential_check_interval: Duration::from_millis(10),
                operation_timeout: Duration::from_secs(2),
                database_connections: 2,
                max_inflight_requests: 16,
                max_subscriptions: 8,
                max_body_bytes: 65536,
            },
            cors_origins: vec![],
            principal_quotas: None,
        },
    )
    .await
    .unwrap();
    let router = host.router();
    let (source, _) = store.managed_head(&alice).await.unwrap();
    // Membership mistakes must fail before acquiring a live stream or executing
    // any member, rather than silently overwriting duplicate keyed results.
    use tower::ServiceExt;
    for queries in [
        json!([]),
        json!([{"key":"","name":"counted","input":{}}]),
        json!([{"key":"same","name":"counted","input":{}},{"key":"same","name":"counted","input":{}}]),
        Json::Array(
            (0..129)
                .map(|i| json!({"key":i.to_string(),"name":"counted","input":{}}))
                .collect(),
        ),
    ] {
        let response = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/functions/subscriptions")
                    .header("Authorization", "Bearer alice")
                    .header("Content-Type", "application/json")
                    .body(axum::body::Body::from(
                        json!({"expected_source":source,"queries":queries}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(host.stats().active_subscriptions, 0);
    let (mut first, mut second) = tokio::join!(
        managed_watch(&router, "alice", source, json!({})),
        managed_watch(&router, "alias", source, json!({}))
    );
    let (a, b) = tokio::join!(first.snapshot(), second.snapshot());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "each joining watch checks member policy immediately"
    );
    let stats = host.stats();
    assert_eq!(stats.active_subscriptions, 2);
    assert_eq!(stats.subscription_groups, 1);
    assert_eq!(stats.snapshots_delivered, 2);
    assert!(stats.snapshot_bytes > 0);
    assert_eq!(a["sequence"], "1");
    assert_eq!(b["sequence"], "1");
    assert_ne!(a["subscription"], b["subscription"]);
    assert_eq!(a["round"], b["round"]);
    for (token, input, identity) in [
        ("bob", json!({}), "bob"),
        ("claims", json!({}), "alice"),
        ("scope", json!({}), "alice"),
        ("alice", json!({"other":true}), "alice"),
    ] {
        let mut isolated = managed_watch(&router, token, source, input.clone()).await;
        let value = isolated.snapshot().await;
        assert_eq!(value["round"]["results"][0]["result"]["identity"], identity);
        assert_eq!(value["round"]["results"][0]["result"]["input"], input);
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        6,
        "scope, principal, claims and arguments must partition shared work"
    );
    let before_policy = policy_calls.load(Ordering::SeqCst);
    // Drive two idle consumers through several query and credential ticks.
    let (idle_a, idle_b) = tokio::join!(
        tokio::time::timeout(Duration::from_millis(350), first.next()),
        tokio::time::timeout(Duration::from_millis(350), second.next()),
    );
    assert!(idle_a.is_err() && idle_b.is_err());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        6,
        "unchanged heads do not retry handlers"
    );
    let refreshes = policy_calls.load(Ordering::SeqCst) - before_policy;
    assert!(
        refreshes == 0,
        "credential ticks must not cause extra database refreshes: {refreshes}"
    );

    let stats = host.stats();
    assert!(stats.coalesced_refreshes > 0);
    assert!(stats.unchanged_refreshes > 0);
    assert!(stats.subscription_refreshes > 0);

    // Empty successes and failures remain members of a single captured round.
    // A later write must replace every member together, including failed reads.
    let response = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/functions/subscriptions")
                .header("Authorization", "Bearer alice")
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(
                    json!({"expected_source":source,"queries":[
                        {"key":"list","name":"counted","input":{}},
                        {"key":"required","name":"counted","input":{"require_nonempty":true}}
                    ]})
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let mut round = ManagedEvents {
        body: response.into_body(),
        buffer: Vec::new(),
    };
    let initial = round.snapshot().await;
    assert_eq!(initial["round"]["results"][0]["key"], "list");
    assert_eq!(initial["round"]["results"][0]["result"]["rows"], json!([]));
    assert_eq!(initial["round"]["results"][1]["error"]["code"], "not_found");
    let before_idle = calls.load(Ordering::SeqCst);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), round.next())
            .await
            .is_err()
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        before_idle,
        "failed members wait for a database change"
    );

    // A write outside the HTTP request lifecycle must still refresh both streams,
    // once, and advance each independent delivery sequence to the receipt fence.
    let mut writer = Registry::new();
    writer
        .mutation("create", Create { fail: false }, Policy)
        .unwrap();
    let receipt = store
        .mutate(
            &writer,
            &alice,
            Uuid::now_v7(),
            "create",
            json!({"title":"visible","ordinal":"1"}),
        )
        .await
        .unwrap();
    let (a, b) = tokio::join!(first.snapshot(), second.snapshot());
    assert_eq!(a["sequence"], "2");
    assert_eq!(b["sequence"], "2");
    assert_eq!(
        a["round"]["revision"],
        receipt.revision.sequence.to_string()
    );
    assert_eq!(a["round"], b["round"]);
    assert_eq!(
        a["round"]["results"][0]["result"]["rows"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let recovered_round = round.snapshot().await;
    assert_eq!(
        recovered_round["round"]["revision"],
        receipt.revision.sequence.to_string()
    );
    assert_eq!(
        recovered_round["round"]["results"][0]["result"]["rows"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        recovered_round["round"]["results"][1]["result"]["rows"],
        recovered_round["round"]["results"][0]["result"]["rows"]
    );
    drop(round);

    // Ordinary server callbacks publish through exactly the same revision head.
    // No function registry or mutation receipt is needed for live invalidation.
    store
        .direct_transaction(alice.scope, true, |_, write| {
            Box::pin(async move {
                write
                    .unwrap()
                    .insert(
                        "entries",
                        RecordValues::from([
                            ("title".into(), Value::String("direct server write".into())),
                            ("owner".into(), Value::String("alice".into())),
                            ("ordinal".into(), Value::Int64(2)),
                        ]),
                    )
                    .await?;
                Ok(((), true))
            })
        })
        .await
        .unwrap();
    let (a, b) = tokio::join!(first.snapshot(), second.snapshot());
    assert_eq!(a["sequence"], "3");
    assert_eq!(b["sequence"], "3");
    assert_eq!(a["round"], b["round"]);
    assert_eq!(
        a["round"]["revision"],
        (receipt.revision.sequence + 1).to_string()
    );
    assert_eq!(
        a["round"]["results"][0]["result"]["rows"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    credentials.0.lock().unwrap().remove("alias");
    let (event, error) = second.next().await;
    assert_eq!(event, "error");
    assert_eq!(error["code"], "unauthorized");
    drop(second);
    assert!(
        tokio::time::timeout(Duration::from_millis(40), first.next())
            .await
            .is_err(),
        "another credential's revocation must not retire the shared query"
    );
    allowed.store(false, Ordering::SeqCst);
    let mut denied = managed_watch(&router, "alice", source, json!({})).await;
    assert_eq!(
        denied.snapshot().await["round"]["results"][0]["error"]["code"],
        "forbidden",
        "joining must recheck member policy immediately"
    );
    assert_eq!(
        first.snapshot().await["round"]["results"][0]["error"]["code"],
        "forbidden",
        "authorization must run even without a database revision change"
    );
    drop(denied);
    drop(first);

    // With every reader gone, the weak registry must release the old cache.
    allowed.store(true, Ordering::SeqCst);
    let mut reconnected = managed_watch(&router, "alice", source, json!({})).await;
    assert_eq!(reconnected.snapshot().await["sequence"], "1");
    // A queued first frame is checked again when polled for delivery.
    let mut queued = managed_watch(&router, "alice", source, json!({})).await;
    credentials.0.lock().unwrap().remove("alice");
    assert_eq!(queued.next().await.1["code"], "unauthorized");
    drop(queued);
    credentials
        .0
        .lock()
        .unwrap()
        .insert("alice".into(), alice.clone());
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!(
            "BEGIN; LOCK TABLE \"{}\".metadata IN ACCESS EXCLUSIVE MODE",
            schema.namespace(),
        ))
        .await
        .unwrap();
    let (event, error) = reconnected.next().await;
    assert_eq!(event, "error");
    assert_eq!(error["code"], "unavailable");
    client.batch_execute("ROLLBACK").await.unwrap();
    // The terminal old body still owns the group. Cancellation must have
    // invalidated its coverage before a new reader can reuse it.
    let mut recovered = managed_watch(&router, "alice", source, json!({})).await;
    assert_eq!(recovered.snapshot().await["sequence"], "1");
    drop(reconnected);
    host.stop();
    use http_body_util::BodyExt;
    assert!(
        tokio::time::timeout(Duration::from_secs(1), recovered.body.frame())
            .await
            .unwrap()
            .is_none()
    );
    drop(recovered);
    let stats = host.stats();
    assert_eq!(stats.active_subscriptions, 0);
    assert_eq!(stats.subscription_groups, 0);
    assert_eq!(stats.database_waiters, 0);
    assert!(stats.operation_deadlines >= 1);
    assert!(stats.stream_failures >= 3);
    client
        .batch_execute(&format!("DROP SCHEMA \"{}\" CASCADE", schema.namespace()))
        .await
        .unwrap();
    connection.abort();
}

// At risk: terminal expiration mapped as absence/unavailability, or cancellation
// leaving inflated pressure gauges and concealing a stuck database pool.
#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn host_reports_expired_receipts_deadlines_and_cancelled_pool_waiters() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let schema = ManagedSchema::new(
        format!("visibility_{}", Uuid::now_v7().simple()),
        generated::schema().unwrap(),
    )
    .unwrap();
    let mut store = ManagedStore::connect(&url(), schema.clone()).await.unwrap();
    store.initialize().await.unwrap();
    let context = Context {
        scope: Uuid::now_v7(),
        principal: Uuid::now_v7(),
        authorization_scope: Uuid::now_v7(),
        identity: "alice".into(),
    };
    store.create_scope(context.scope).await.unwrap();
    let mut registry = Registry::new();
    registry.query("deployed.list", List, Policy).unwrap();
    registry
        .mutation("deployed.create", Create { fail: false }, Policy)
        .unwrap();
    let request_id = Uuid::now_v7();
    let input = json!({"title":"committed","ordinal":"1"});
    let ticket = store.issue_ticket(&context).await.unwrap();
    let (source, _) = store.managed_head(&context).await.unwrap();
    let receipt = store
        .mutate_with_ticket(
            &registry,
            &context,
            ticket.id,
            request_id,
            source,
            "deployed.create",
            input.clone(),
        )
        .await
        .unwrap();
    let (db, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    db.batch_execute(&format!(
        "UPDATE \"{}\".receipts SET created_at=clock_timestamp()-interval '1 day'",
        schema.namespace()
    ))
    .await
    .unwrap();
    store
        .compact_receipts(context.scope, receipt.source, Duration::from_secs(3600), 1)
        .await
        .unwrap();
    let credentials = Arc::new(Credentials(Mutex::new(HashMap::from([(
        "alice".into(),
        context.clone(),
    )]))));
    let host = ManagedHost::connect(
        &url(),
        schema.clone(),
        registry,
        credentials,
        Options {
            budgets: HostOptions {
                database_connections: 1,
                operation_timeout: Duration::from_millis(300),
                poll_interval: Duration::from_millis(20),
                credential_check_interval: Duration::from_secs(1),
                max_inflight_requests: 4,
                max_subscriptions: 2,
                max_body_bytes: 65536,
            },
            cors_origins: vec![],
            principal_quotas: None,
        },
    )
    .await
    .unwrap();
    let router = host.router();
    let request = |uri: String, body: Option<Json>| {
        let builder = Request::builder()
            .method(if body.is_some() { "POST" } else { "GET" })
            .uri(uri)
            .header("Authorization", "Bearer alice")
            .header("Content-Type", "application/json");
        builder
            .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
            .unwrap()
    };
    for retry in [false, true] {
        let (uri, body) = if retry {
            (
                "/functions/mutate".into(),
                Some(
                    json!({"ticket":ticket.id,"request_id":request_id,"name":"deployed.create","input":input,"expected_source":receipt.source}),
                ),
            )
        } else {
            (
                format!(
                    "/functions/receipts/{request_id}?source={}&ticket={}",
                    receipt.source, ticket.id
                ),
                None,
            )
        };
        let response = router.clone().oneshot(request(uri, body)).await.unwrap();
        assert_eq!(response.status(), StatusCode::GONE);
        assert_eq!(
            serde_json::from_slice::<Json>(&to_bytes(response.into_body(), 1024).await.unwrap())
                .unwrap()["code"],
            "receipt_expired"
        );
    }
    assert_eq!(host.stats().expired_receipts, 2);
    assert_eq!(store.scope_usage(context.scope).await.unwrap().rows, 1);
    db.batch_execute(&format!(
        "BEGIN; LOCK TABLE \"{}\".metadata IN ACCESS EXCLUSIVE MODE",
        schema.namespace()
    ))
    .await
    .unwrap();
    let query = || {
        request(
            "/functions/query".into(),
            Some(json!({"name":"deployed.list","input":null,"expected_source":receipt.source})),
        )
    };
    let first = tokio::spawn(router.clone().oneshot(query()));
    tokio::time::timeout(Duration::from_secs(1), async {
        while host.stats().database_in_use != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let second = tokio::spawn(router.clone().oneshot(query()));
    tokio::time::timeout(Duration::from_secs(1), async {
        while host.stats().database_waiters != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    second.abort();
    let _ = second.await;
    assert_eq!(host.stats().database_waiters, 0);
    assert_eq!(host.stats().requests_cancelled, 1);
    assert_eq!(
        first.await.unwrap().unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    let mutation = request(
        "/functions/mutate".into(),
        Some(
            json!({"ticket":ticket.id,"request_id":Uuid::now_v7(),"name":"deployed.create","input":{"title":"blocked","ordinal":"2"},"expected_source":receipt.source}),
        ),
    );
    let response = router.clone().oneshot(mutation).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        serde_json::from_slice::<Json>(&to_bytes(response.into_body(), 1024).await.unwrap())
            .unwrap()["code"],
        "outcome_unknown"
    );
    db.batch_execute("ROLLBACK").await.unwrap();
    let stats = host.stats();
    assert_eq!(stats.operation_deadlines, 2);
    assert_eq!(stats.unknown_outcomes, 1);
    assert_eq!(stats.server_error_responses, 2);
    assert_eq!(stats.active_requests, 0);
    assert_eq!(stats.database_in_use, 0);
    assert_eq!(
        stats.requests_started,
        stats.requests_completed + stats.requests_cancelled
    );
    assert!(host.ready().await);
    // No operator data or cleanup endpoints are installed on the function router.
    assert_eq!(
        router
            .oneshot(request("/functions/metrics".into(), None))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    host.stop();
    db.batch_execute(&format!("DROP SCHEMA \"{}\" CASCADE", schema.namespace()))
        .await
        .unwrap();
    connection.abort();
}

// At risk: the transport rejecting anonymous/cookie requests before the trusted
// verifier sees them, accepting client-supplied identity pins, or sharing a live
// result after the provider revokes credentials without a database revision.
#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn cookie_and_anonymous_auth_use_provider_context_and_retire_revoked_live_delivery() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use http_body_util::BodyExt;
    use std::sync::atomic::AtomicUsize;
    use tower::ServiceExt;

    struct CookieAuth {
        anonymous: Context<String>,
        sessions: Mutex<HashMap<String, Context<String>>>,
    }
    impl Authenticator<String> for CookieAuth {
        fn authenticate<'a>(
            &'a self,
            headers: &'a CredentialHeaders,
        ) -> AuthenticationFuture<'a, String> {
            Box::pin(async move {
                let mut cookies = headers.get_all("cookie").iter();
                let Some(cookie) = cookies.next() else {
                    // Anonymous is an explicit provider decision, not a transport
                    // fallback after invalid or expired credentials.
                    return if headers.contains_key("authorization") {
                        Err(AuthenticationError::InvalidCredentials)
                    } else {
                        Ok(self.anonymous.clone())
                    };
                };
                if cookies.next().is_some() {
                    return Err(AuthenticationError::InvalidCredentials);
                }
                let cookie = cookie
                    .to_str()
                    .map_err(|_| AuthenticationError::InvalidCredentials)?;
                self.sessions
                    .lock()
                    .unwrap()
                    .get(cookie)
                    .cloned()
                    .ok_or(AuthenticationError::InvalidCredentials)
            })
        }
    }
    let schema = ManagedSchema::new(
        format!("cookie_auth_{}", Uuid::now_v7().simple()),
        generated::schema().unwrap(),
    )
    .unwrap();
    let mut store = ManagedStore::connect(&url(), schema.clone()).await.unwrap();
    store.initialize().await.unwrap();
    let anonymous = Context {
        scope: Uuid::now_v7(),
        principal: Uuid::now_v7(),
        authorization_scope: Uuid::now_v7(),
        identity: "anonymous".to_owned(),
    };
    store.create_scope(anonymous.scope).await.unwrap();
    let alice = Context {
        principal: Uuid::now_v7(),
        authorization_scope: Uuid::now_v7(),
        identity: "alice".into(),
        ..anonymous.clone()
    };
    let credentials = Arc::new(CookieAuth {
        anonymous: anonymous.clone(),
        sessions: Mutex::new(HashMap::from([("session=alice".into(), alice.clone())])),
    });
    // Commit before deployment; the new query-only registry does not contain
    // this mutation. Receipt delivery must depend on verified ownership, not on
    // the current declaration being available or its application policy.
    let ticket = store.issue_ticket(&alice).await.unwrap();
    let (source, _) = store.managed_head(&alice).await.unwrap();
    let original = {
        let mut original_registry = Registry::new();
        original_registry
            .mutation("deployed.create", Create { fail: false }, Policy)
            .unwrap();
        store
            .mutate_with_ticket(
                &original_registry,
                &alice,
                ticket.id,
                Uuid::now_v7(),
                source,
                "deployed.create",
                json!({"title":"retained response", "ordinal":"1"}),
            )
            .await
            .unwrap()
    };
    let mut registry = Registry::new();
    registry
        .query(
            "counted",
            CountedList(Arc::new(AtomicUsize::new(0))),
            Policy,
        )
        .unwrap();
    let host = ManagedHost::connect(
        &url(),
        schema.clone(),
        registry,
        credentials.clone(),
        Options {
            budgets: HostOptions {
                database_connections: 2,
                operation_timeout: Duration::from_secs(2),
                poll_interval: Duration::from_millis(100),
                credential_check_interval: Duration::from_millis(10),
                max_inflight_requests: 8,
                max_subscriptions: 4,
                max_body_bytes: 65536,
            },
            cors_origins: vec!["https://app.example".into()],
            principal_quotas: None,
        },
    )
    .await
    .unwrap();
    let router = host.router();
    let (source, _) = store.managed_head(&anonymous).await.unwrap();
    let confirm = |cookie: Option<&str>| {
        let mut request = Request::builder().uri(format!(
            "/functions/receipts/{}?source={}&ticket={}",
            original.request_id, original.source, ticket.id
        ));
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        router.clone().oneshot(request.body(Body::empty()).unwrap())
    };
    let response = confirm(Some("session=alice")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let delivered: Json =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(delivered["result"], original.result);
    let response = confirm(None).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Json>(&response.into_body().collect().await.unwrap().to_bytes())
            .unwrap(),
        Json::Null,
        "the anonymous principal cannot receive Alice's response"
    );
    for (cookie, expected) in [(None, &anonymous), (Some("session=alice"), &alice)] {
        let mut request = Request::builder()
            .uri("/functions/session")
            .header("Origin", "https://app.example");
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        let response = router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["access-control-allow-origin"],
            "https://app.example"
        );
        assert_eq!(
            response.headers()["access-control-allow-credentials"],
            "true"
        );
        let session: Json =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(session["principal"], expected.principal.to_string());
        assert_eq!(
            session["authorization_scope"],
            expected.authorization_scope.to_string()
        );
        let mut request = Request::builder()
            .method("POST")
            .uri("/functions/query")
            .header("Content-Type", "application/json")
            .header("x-hulladb-principal", expected.principal.to_string());
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        let response = router
            .clone()
            .oneshot(
                request
                    .body(Body::from(
                        json!({"name":"counted","input":{},"expected_source":source}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let snapshot: Json =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(snapshot["result"]["identity"], expected.identity);
    }
    for request in [
        Request::builder().header("Cookie", "session=expired"),
        Request::builder().header("Authorization", "Bearer invalid"),
        Request::builder()
            .header("Cookie", "session=alice")
            .header("x-hulladb-principal", anonymous.principal.to_string()),
        Request::builder().header("x-hulladb-principal", alice.principal.to_string()),
        Request::builder()
            .header("Cookie", "session=alice")
            .header("Origin", "https://untrusted.example"),
    ] {
        let response = router
            .clone()
            .oneshot(
                request
                    .uri("/functions/session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let open = || {
        router.clone().oneshot(
            Request::builder()
                .method("POST")
                .uri("/functions/subscriptions")
                .header("Cookie", "session=alice")
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({"queries":[{"key":"list","name":"counted","input":{}}],"expected_source":source}).to_string(),
                ))
                .unwrap(),
        )
    };
    let response = open().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut live = ManagedEvents {
        body: response.into_body(),
        buffer: Vec::new(),
    };
    assert_eq!(
        live.snapshot().await["round"]["results"][0]["result"]["identity"],
        "alice"
    );
    // Same principal, changed verified claims: stop the old subscription even
    // with no writes and without trusting client-supplied auth-change messages.
    credentials.sessions.lock().unwrap().insert(
        "session=alice".into(),
        Context {
            authorization_scope: Uuid::now_v7(),
            ..alice.clone()
        },
    );
    assert_eq!(live.next().await.1["code"], "unauthorized");
    drop(live);
    assert_eq!(
        confirm(Some("session=alice")).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let response = open().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut live = ManagedEvents {
        body: response.into_body(),
        buffer: Vec::new(),
    };
    live.snapshot().await;
    credentials.sessions.lock().unwrap().clear();
    assert_eq!(
        confirm(Some("session=alice")).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(live.next().await.1["code"], "unauthorized");
    drop(live);
    // An explicitly invalid cookie never downgrades to anonymous access.
    let response = open().await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    host.stop();
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!("DROP SCHEMA \"{}\" CASCADE", schema.namespace()))
        .await
        .unwrap();
    connection.abort();
}

// A lost error response must be just as recoverable as a lost success. Renewing
// the connection must not give an old request a second execution identity.
#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn ticket_http_flow_preserves_terminal_outcomes_across_renewal_and_ack() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let schema = ManagedSchema::new(
        format!("ticket_http_{}", Uuid::now_v7().simple()),
        generated::schema().unwrap(),
    )
    .unwrap();
    let mut store = ManagedStore::connect(&url(), schema.clone()).await.unwrap();
    store.initialize().await.unwrap();
    let context = Context {
        scope: Uuid::now_v7(),
        principal: Uuid::now_v7(),
        authorization_scope: Uuid::now_v7(),
        identity: "alice".into(),
    };
    store.create_scope(context.scope).await.unwrap();
    let mut registry = Registry::new();
    registry
        .mutation("create", Create { fail: false }, Policy)
        .unwrap();
    registry
        .mutation("fail", Create { fail: true }, Policy)
        .unwrap();
    let host = ManagedHost::connect(
        &url(),
        schema.clone(),
        registry,
        Arc::new(Credentials(Mutex::new(HashMap::from([(
            "alice".into(),
            context.clone(),
        )])))),
        Options {
            budgets: HostOptions {
                database_connections: 3,
                operation_timeout: Duration::from_secs(5),
                poll_interval: Duration::from_millis(20),
                credential_check_interval: Duration::from_secs(1),
                max_inflight_requests: 8,
                max_subscriptions: 2,
                max_body_bytes: 65536,
            },
            cors_origins: vec![],
            principal_quotas: None,
        },
    )
    .await
    .unwrap();
    let router = host.router();
    let call = |uri: String, body: Option<Json>| {
        router.clone().oneshot(
            Request::builder()
                .method(if body.is_some() { "POST" } else { "GET" })
                .uri(uri)
                .header("Authorization", "Bearer alice")
                .header("Content-Type", "application/json")
                .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
                .unwrap(),
        )
    };
    let started = std::time::Instant::now();
    let session = call("/functions/session".into(), None).await.unwrap();
    assert_eq!(session.status(), StatusCode::OK);
    let session: Json =
        serde_json::from_slice(&to_bytes(session.into_body(), 4096).await.unwrap()).unwrap();
    let ticket = session["ticket"]["id"].as_str().unwrap();
    let source = session["source"].as_str().unwrap();
    assert!(
        session["ticket"]["renew_after_ms"].as_u64().unwrap()
            < session["ticket"]["expires_in_ms"].as_u64().unwrap()
    );
    let setup_time = started.elapsed();
    let started = std::time::Instant::now();
    let renewal = call("/functions/session".into(), None).await.unwrap();
    let renewal: Json =
        serde_json::from_slice(&to_bytes(renewal.into_body(), 4096).await.unwrap()).unwrap();
    let renewal_time = started.elapsed();
    assert_ne!(renewal["ticket"]["id"], ticket);
    let failed_id = Uuid::now_v7();
    let failed = json!({"ticket":ticket,"request_id":failed_id,"expected_source":source,"name":"fail","input":{"title":"rolled back","ordinal":"1"}});
    // Discard the response body, as when a connection drops after rollback.
    let response = call("/functions/mutate".into(), Some(failed.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    drop(response);
    let lookup = format!("/functions/receipts/{failed_id}?source={source}&ticket={ticket}");
    for (uri, body) in [
        (lookup.clone(), None),
        ("/functions/mutate".into(), Some(failed.clone())),
    ] {
        let response = call(uri, body).await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body: Json =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["code"], "conflict");
    }
    assert_eq!(store.scope_usage(context.scope).await.unwrap().rows, 0);
    let mut rebound = failed.clone();
    rebound["ticket"] = renewal["ticket"]["id"].clone();
    let response = call("/functions/mutate".into(), Some(rebound))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        serde_json::from_slice::<Json>(&to_bytes(response.into_body(), 4096).await.unwrap())
            .unwrap()["code"],
        "request_id_reused"
    );
    // Strict parsing prevents ambiguous original-ticket identity selection.
    assert_eq!(
        call(format!("{lookup}&ticket={ticket}"), None)
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    let success_id = Uuid::now_v7();
    let success = json!({"ticket":ticket,"request_id":success_id,"expected_source":source,"name":"create","input":{"title":"committed","ordinal":"2"}});
    let response = call("/functions/mutate".into(), Some(success.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let original: Json =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    let lookup = format!("/functions/receipts/{success_id}?source={source}&ticket={ticket}");
    let response = call(lookup.clone(), None).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Json>(&to_bytes(response.into_body(), 4096).await.unwrap())
            .unwrap(),
        original
    );
    for _ in 0..2 {
        assert_eq!(
            call(
                format!("/functions/receipts/{success_id}/ack"),
                Some(json!({"source":source,"ticket":ticket}))
            )
            .await
            .unwrap()
            .status(),
            StatusCode::OK
        );
    }
    assert_eq!(call(lookup, None).await.unwrap().status(), StatusCode::GONE);
    assert_eq!(
        call("/functions/mutate".into(), Some(success.clone()))
            .await
            .unwrap()
            .status(),
        StatusCode::GONE
    );
    assert_eq!(store.scope_usage(context.scope).await.unwrap().rows, 1);
    let (db, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let sizes = db.query_one(&format!("SELECT (SELECT pg_column_size(t) FROM \"{}\".tickets t WHERE id=$1), (SELECT pg_column_size(a) FROM \"{}\".admissions a WHERE request_id=$2), (SELECT pg_column_size(r) FROM \"{}\".receipts r WHERE request_id=$2)", schema.namespace(), schema.namespace(), schema.namespace()), &[&ticket.parse::<Uuid>().unwrap(), &success_id]).await.unwrap();
    eprintln!(
        "ticket HTTP handler setup={setup_time:?} renewal={renewal_time:?}; tuple bytes ticket={} admission={} acknowledged receipt={} (excludes indexes/WAL)",
        sizes.get::<_, i32>(0),
        sizes.get::<_, i32>(1),
        sizes.get::<_, i32>(2)
    );
    // Force retention boundaries, then let the real host worker retire/purge.
    db.execute(&format!("UPDATE \"{}\".tickets SET expires_at=clock_timestamp()-interval '1 second' WHERE id=$1", schema.namespace()), &[&ticket.parse::<Uuid>().unwrap()]).await.unwrap();
    db.execute(&format!("UPDATE \"{}\".admissions SET purge_after=clock_timestamp()-interval '1 second',result_until=clock_timestamp()-interval '1 second' WHERE request_id=$1", schema.namespace()), &[&success_id]).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let count: i64 = db
                .query_one(
                    &format!(
                        "SELECT count(*) FROM \"{}\".admissions WHERE request_id=$1",
                        schema.namespace()
                    ),
                    &[&success_id],
                )
                .await
                .unwrap()
                .get(0);
            if count == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let response = call("/functions/mutate".into(), Some(success))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::GONE);
    assert_eq!(
        serde_json::from_slice::<Json>(&to_bytes(response.into_body(), 4096).await.unwrap())
            .unwrap()["code"],
        "ticket_expired"
    );
    assert_eq!(store.scope_usage(context.scope).await.unwrap().rows, 1);
    host.stop();
    db.batch_execute(&format!("DROP SCHEMA \"{}\" CASCADE", schema.namespace()))
        .await
        .unwrap();
    connection.abort();
}
