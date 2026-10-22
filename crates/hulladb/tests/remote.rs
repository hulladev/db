#![cfg(feature = "remote")]
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{StreamExt, stream};
use hulladb::{
    ClientFailure, Error, QueryState, Runtime, Uuid,
    remote::{Endpoint, Options, Partition, RecoveryStore, RemoteRuntime, Session},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, AtomicU8, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Notify, task::JoinHandle};

#[cfg(feature = "server")]
use hulladb::{MutationContext, QueryContext};
#[hulladb::query(name = "notes.value")]
async fn value(_: QueryContext<'_, ()>) -> Result<String, Error> {
    Ok("server-only".into())
}
#[hulladb::mutation(name = "notes.write")]
async fn write(_: MutationContext<'_, ()>, input: String) -> Result<String, Error> {
    Ok(input)
}

struct Script {
    chunks: Vec<Vec<u8>>,
    open: bool,
    gate_after_first: bool,
}
struct Peer {
    session: Session,
    source: Mutex<Uuid>,
    revision: AtomicI64,
    mode: AtomicU8,
    deny: AtomicU8,
    commits: Mutex<HashMap<Uuid, Value>>,
    attempts: Mutex<Vec<Uuid>>,
    tickets: Mutex<HashMap<Uuid, Uuid>>,
    issued: AtomicUsize,
    renewal_ms: AtomicUsize,
    scripts: Mutex<VecDeque<Script>>,
    streams: AtomicUsize,
    entered: Notify,
    release: Notify,
}
impl Peer {
    fn source(&self) -> Uuid {
        *self.source.lock().unwrap()
    }
    fn snapshot(&self, result: Value) -> Value {
        json!({"source":self.source(),"scope":self.session.scope,"principal":self.session.principal,"authorization_scope":self.session.authorization_scope,"revision":self.revision.load(Ordering::SeqCst).to_string(),"result":result})
    }
    fn frame(&self, subscription: Uuid, sequence: i64, result: Value) -> Vec<u8> {
        let mut round = self.snapshot(result);
        let result = round.as_object_mut().unwrap().remove("result").unwrap();
        round["results"] = json!([{ "key": "query", "result": result }]);
        format!(
            "event: round\r\nid: {subscription}:{sequence}\r\ndata: {}\r\n\r\n",
            json!({"subscription":subscription,"sequence":sequence.to_string(),"round":round})
        )
        .into_bytes()
    }
    fn enqueue(&self, frames: Vec<Vec<u8>>, open: bool) {
        self.scripts.lock().unwrap().push_back(Script {
            chunks: frames,
            open,
            gate_after_first: false,
        });
    }
}
struct Fixture {
    peer: Arc<Peer>,
    endpoint: Endpoint,
    task: JoinHandle<()>,
    path: std::path::PathBuf,
}
impl Fixture {
    async fn new() -> Self {
        let peer = Arc::new(Peer {
            session: Session {
                scope: Uuid::now_v7(),
                principal: Uuid::now_v7(),
                authorization_scope: Uuid::now_v7(),
            },
            source: Mutex::new(Uuid::now_v7()),
            revision: AtomicI64::new(0),
            mode: AtomicU8::new(0),
            deny: AtomicU8::new(0),
            commits: Mutex::new(HashMap::new()),
            attempts: Mutex::new(vec![]),
            tickets: Mutex::new(HashMap::new()),
            issued: AtomicUsize::new(0),
            renewal_ms: AtomicUsize::new(30000),
            scripts: Mutex::new(VecDeque::new()),
            streams: AtomicUsize::new(0),
            entered: Notify::new(),
            release: Notify::new(),
        });
        let app = Router::new()
            .route("/functions/session", get(head))
            .route("/functions/query", post(query))
            .route("/functions/mutate", post(mutate))
            .route("/functions/receipts/{id}", get(receipt))
            .route(
                "/functions/receipts/{id}/ack",
                post(|| async { Json(Value::Null) }),
            )
            .route("/functions/subscriptions", post(subscribe))
            .with_state(peer.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = Endpoint::local(listener.local_addr().unwrap()).unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            peer,
            endpoint,
            task,
            path: std::env::temp_dir().join(format!("function-client-{}.sqlite", Uuid::now_v7())),
        }
    }
    async fn store(&self) -> RecoveryStore {
        RecoveryStore::open(&self.path).await.unwrap()
    }
    fn options(&self) -> Options {
        let mut options = Options::default();
        options.transport.reconnect_delay = Duration::from_millis(10);
        options.transport.request_timeout = Duration::from_millis(250);
        options.transport.stream_idle_timeout = Duration::from_secs(1);
        options
    }
    async fn connect(&self) -> Arc<RemoteRuntime> {
        Arc::new(
            RemoteRuntime::connect(
                self.endpoint.clone(),
                "opaque-secret",
                self.peer.session,
                self.store().await,
                self.options(),
            )
            .await
            .unwrap(),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}
fn rejected() -> Response {
    (StatusCode::FORBIDDEN, Json(json!({"code":"forbidden"}))).into_response()
}
fn unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"code":"outcome_unknown"})),
    )
        .into_response()
}
fn identity(peer: &Peer, headers: &HeaderMap) {
    assert_eq!(headers["x-hulladb-scope"], peer.session.scope.to_string());
    assert_eq!(
        headers["x-hulladb-principal"],
        peer.session.principal.to_string()
    );
    assert_eq!(
        headers["x-hulladb-authorization-scope"],
        peer.session.authorization_scope.to_string()
    );
}
async fn head(State(peer): State<Arc<Peer>>, headers: HeaderMap) -> Response {
    identity(&peer, &headers);
    if peer.deny.load(Ordering::SeqCst) == 2 {
        return rejected();
    }
    let mut head = peer.snapshot(Value::Null);
    head.as_object_mut().unwrap().remove("result");
    peer.issued.fetch_add(1, Ordering::SeqCst);
    head["ticket"] = json!({"id":Uuid::now_v7(),"expires_in_ms":60000,"renew_after_ms":peer.renewal_ms.load(Ordering::SeqCst)});
    Json(head).into_response()
}
async fn query(State(peer): State<Arc<Peer>>, headers: HeaderMap) -> Response {
    identity(&peer, &headers);
    if peer.deny.load(Ordering::SeqCst) != 0 {
        return rejected();
    };
    Json(peer.snapshot(json!("from-query"))).into_response()
}
async fn mutate(
    State(peer): State<Arc<Peer>>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> Response {
    identity(&peer, &headers);
    let id = Uuid::parse_str(input["request_id"].as_str().unwrap()).unwrap();
    let ticket = Uuid::parse_str(input["ticket"].as_str().unwrap()).unwrap();
    peer.tickets.lock().unwrap().insert(id, ticket);
    peer.attempts.lock().unwrap().push(id);
    if peer.mode.load(Ordering::SeqCst) == 5 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"code":"quota_exceeded"})),
        )
            .into_response();
    }
    if peer.mode.load(Ordering::SeqCst) == 6 {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"code":"capacity"})),
        )
            .into_response();
    }
    if peer.mode.load(Ordering::SeqCst) == 3 {
        return unavailable();
    }
    let receipt = {
        let mut commits = peer.commits.lock().unwrap();
        commits.entry(id).or_insert_with(||{
        let revision=peer.revision.fetch_add(1,Ordering::SeqCst)+1;
        json!({"request_id":id,"source":peer.source(),"scope":peer.session.scope,"revision":revision.to_string(),"result":if peer.mode.load(Ordering::SeqCst)==4 {json!(42)} else {input["input"].clone()}})
    }).clone()
    };
    peer.entered.notify_one();
    if peer.mode.load(Ordering::SeqCst) == 2 {
        peer.release.notified().await;
    }
    if peer.mode.load(Ordering::SeqCst) == 1 {
        return unavailable();
    }
    Json(receipt).into_response()
}
async fn receipt(
    State(peer): State<Arc<Peer>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    uri: Uri,
) -> Response {
    assert!(
        uri.query()
            .unwrap()
            .contains(&format!("ticket={}", peer.tickets.lock().unwrap()[&id]))
    );
    identity(&peer, &headers);
    if peer.deny.load(Ordering::SeqCst) == 5 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"code":"execution_limit"})),
        )
            .into_response();
    }
    if peer.deny.load(Ordering::SeqCst) == 4 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"code":"execution_failed"})),
        )
            .into_response();
    }
    if peer.deny.load(Ordering::SeqCst) == 3 {
        return (StatusCode::GONE, Json(json!({"code":"receipt_expired"}))).into_response();
    }
    if peer.deny.load(Ordering::SeqCst) != 0 {
        return rejected();
    };
    Json(
        peer.commits
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .unwrap_or(Value::Null),
    )
    .into_response()
}
async fn subscribe(
    State(peer): State<Arc<Peer>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert_eq!(
        body,
        json!({"expected_source":peer.source(),"queries":[{"key":"query","name":"notes.value","input":null}]})
    );
    identity(&peer, &headers);
    peer.streams.fetch_add(1, Ordering::SeqCst);
    let script = peer.scripts.lock().unwrap().pop_front();
    match script {
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"code":"unavailable"})),
        )
            .into_response(),
        Some(script) => {
            let body = stream::iter(
                script
                    .chunks
                    .into_iter()
                    .enumerate()
                    .map(|(index, bytes)| (index, Bytes::from(bytes))),
            )
            .then({
                let peer = peer.clone();
                move |(index, bytes)| {
                    let peer = peer.clone();
                    async move {
                        if script.gate_after_first && index > 0 {
                            peer.release.notified().await;
                        }
                        Ok::<_, std::convert::Infallible>(bytes)
                    }
                }
            });
            let body: Body = if script.open {
                Body::from_stream(body.chain(stream::pending()))
            } else {
                Body::from_stream(body.chain(stream::once(async move {
                    peer.release.notified().await;
                    Ok::<_, std::convert::Infallible>(Bytes::new())
                })))
            };
            ([("content-type", "text/event-stream")], body).into_response()
        }
    }
}
async fn wait_ready(subscription: &mut hulladb::Subscription<String>, connected: bool) -> String {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match &*subscription.state() {
                QueryState::Ready {
                    data,
                    connected: actual,
                    ..
                } if *actual == connected => return data.clone(),
                QueryState::Error(error) => panic!("watch failed: {error}"),
                _ => {}
            }
            subscription.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn lost_ack_uses_original_receipt_and_persists_confirmation() {
    let f = Fixture::new().await;
    let runtime = f.connect().await;
    f.peer.mode.store(1, Ordering::SeqCst);
    let output = hulladb::scope(runtime.clone(), write("hello".into()))
        .await
        .unwrap();
    assert_eq!(output, "hello");
    assert_eq!(f.peer.attempts.lock().unwrap().len(), 1);
    assert_eq!(f.peer.commits.lock().unwrap().len(), 1);
    assert!(runtime.pending().await.unwrap().is_empty());
    let receipts = f
        .store()
        .await
        .completed(runtime.partition())
        .await
        .unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].receipt.result, "hello");
    drop(runtime);
    let reopened = f.connect().await;
    assert!(
        reopened
            .recover_pending(&[write::contract()])
            .await
            .unwrap()
            .is_empty()
    );
    f.store()
        .await
        .forget_receipt(reopened.partition(), receipts[0].receipt.request_id)
        .await
        .unwrap();
    assert!(
        f.store()
            .await
            .completed(reopened.partition())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn cancellation_and_denied_recovery_keep_the_same_durable_intent() {
    let f = Fixture::new().await;
    let runtime = f.connect().await;
    f.peer.mode.store(2, Ordering::SeqCst);
    let call = tokio::spawn(hulladb::scope(runtime.clone(), write("cancelled".into())));
    f.peer.entered.notified().await;
    let pending = runtime.pending().await.unwrap();
    assert_eq!(pending.len(), 1);
    let id = pending[0].request_id;
    call.abort();
    let _ = call.await;
    drop(runtime);
    let reopened = f.connect().await;
    f.peer.deny.store(1, Ordering::SeqCst);
    assert!(matches!(
        reopened.recover_pending(&[write::contract()]).await,
        Err(Error::Forbidden)
    ));
    assert_eq!(reopened.pending().await.unwrap()[0].request_id, id);
    drop(reopened);
    f.peer.deny.store(0, Ordering::SeqCst);
    let reopened = f.connect().await;
    let receipts = reopened
        .recover_pending(&[write::contract()])
        .await
        .unwrap();
    assert_eq!(receipts[0].request_id, id);
    assert_eq!(receipts[0].result, "cancelled");
    assert_eq!(f.peer.attempts.lock().unwrap().len(), 1);
    let other = Session {
        principal: Uuid::now_v7(),
        ..f.peer.session
    };
    let other = Partition::new(&f.endpoint, other);
    assert!(f.store().await.pending(&other).await.unwrap().is_empty());
    assert!(f.store().await.completed(&other).await.unwrap().is_empty());
}

#[tokio::test]
async fn missing_recovery_never_resubmits_and_source_changes_require_reconciliation() {
    let f = Fixture::new().await;
    let mut options = f.options();
    options.recovery_attempts = 1;
    let runtime = Arc::new(
        RemoteRuntime::connect(
            f.endpoint.clone(),
            "secret",
            f.peer.session,
            f.store().await,
            options,
        )
        .await
        .unwrap(),
    );
    f.peer.mode.store(3, Ordering::SeqCst);
    assert!(matches!(
        hulladb::scope(runtime.clone(), write("retry".into())).await,
        Err(Error::CommitOutcomeUnknown)
    ));
    let original = runtime.pending().await.unwrap()[0].clone();
    f.peer.mode.store(0, Ordering::SeqCst);
    assert!(matches!(
        runtime.recover_pending(&[]).await,
        Err(Error::InvalidInput(_))
    ));
    let incompatible = hulladb::MutationContract::new::<bool, String>(write::NAME);
    assert!(matches!(
        runtime.recover_pending(&[incompatible]).await,
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(f.peer.attempts.lock().unwrap().len(), 1);
    assert!(matches!(
        runtime.recover_pending(&[write::contract()]).await,
        Err(Error::CommitOutcomeUnknown)
    ));
    assert_eq!(
        f.peer.attempts.lock().unwrap().as_slice(),
        &[original.request_id]
    );
    assert_eq!(
        runtime.pending().await.unwrap()[0].request_id,
        original.request_id
    );
    f.peer.mode.store(3, Ordering::SeqCst);
    assert!(
        hulladb::scope(runtime.clone(), write("old-source".into()))
            .await
            .is_err()
    );
    let partition = runtime.partition().clone();
    let pending = runtime.pending().await.unwrap();
    drop(runtime);
    *f.peer.source.lock().unwrap() = Uuid::now_v7();
    assert!(matches!(
        RemoteRuntime::connect(
            f.endpoint.clone(),
            "secret",
            f.peer.session,
            f.store().await,
            options
        )
        .await,
        Err(Error::SourceMismatch)
    ));
    assert!(matches!(
        f.store()
            .await
            .reconcile_source(&partition, f.peer.source())
            .await,
        Err(Error::CommitOutcomeUnknown)
    ));
    for intent in pending {
        f.store()
            .await
            .discard_intent(&partition, intent.request_id)
            .await
            .unwrap();
    }
    f.store()
        .await
        .reconcile_source(&partition, f.peer.source())
        .await
        .unwrap();
    let _reopened = f.connect().await;
}

#[tokio::test]
async fn invalid_typed_mutation_result_cannot_erase_its_intent() {
    let f = Fixture::new().await;
    let runtime = f.connect().await;
    f.peer.mode.store(4, Ordering::SeqCst);
    assert!(matches!(
        hulladb::scope(runtime.clone(), write("bad".into())).await,
        Err(Error::Client(ClientFailure::Protocol))
    ));
    assert_eq!(runtime.pending().await.unwrap().len(), 1);
    assert!(
        f.store()
            .await
            .completed(runtime.partition())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn sse_reconnect_hydrates_stale_results_and_requires_live_visibility() {
    let f = Fixture::new().await;
    let runtime = f.connect().await;
    let frame = f.peer.frame(Uuid::now_v7(), 1, json!("héllo"));
    // Split every byte, including UTF-8 and CRLF delimiters.
    f.peer
        .enqueue(frame.into_iter().map(|byte| vec![byte]).collect(), true);
    let mut subscription = hulladb::scope(runtime.clone(), value::watch())
        .await
        .unwrap();
    assert_eq!(wait_ready(&mut subscription, true).await, "héllo");
    let receipt = runtime
        .mutation("notes.write", json!("next"))
        .await
        .unwrap();
    assert!(
        subscription
            .wait_visible(&receipt, Duration::from_millis(20))
            .await
            .is_err()
    );
    drop(subscription);
    drop(runtime);
    let reopened = f.connect().await;
    let mut subscription = hulladb::scope(reopened.clone(), value::watch())
        .await
        .unwrap();
    assert_eq!(wait_ready(&mut subscription, false).await, "héllo");
    assert!(
        subscription
            .wait_visible(&receipt, Duration::from_millis(20))
            .await
            .is_err()
    );
    f.peer
        .enqueue(vec![f.peer.frame(Uuid::now_v7(), 1, json!("new"))], false);
    assert_eq!(wait_ready(&mut subscription, true).await, "new");
    subscription
        .wait_visible(&receipt, Duration::from_secs(1))
        .await
        .unwrap();
    f.peer.release.notify_one();
    assert_eq!(wait_ready(&mut subscription, false).await, "new");
    reopened.retire().await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if matches!(*subscription.state(), QueryState::Error(_)) {
                break;
            }
            subscription.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        hulladb::scope(reopened, value()).await,
        Err(Error::Client(ClientFailure::Closed))
    ));
}

#[tokio::test]
async fn malformed_or_oversized_sse_is_terminal_and_never_cached() {
    for malformed in 0..10 {
        let f = Fixture::new().await;
        let mut options = f.options();
        if malformed == 3 {
            options.transport.max_event_bytes = 64;
        }
        let runtime = Arc::new(
            RemoteRuntime::connect(
                f.endpoint.clone(),
                "secret",
                f.peer.session,
                f.store().await,
                options,
            )
            .await
            .unwrap(),
        );
        let subscription = Uuid::now_v7();
        let frames = match malformed {
            0 => vec![f.peer.frame(subscription, 2, json!("skipped"))],
            1 => vec![
                f.peer.frame(subscription, 1, json!("first")),
                f.peer.frame(subscription, 1, json!("duplicate")),
            ],
            2 => vec![f.peer.frame(subscription, 1, json!(42))],
            3 => vec![f.peer.frame(subscription, 1, json!("oversized"))],
            _ => {
                let mut round = f.peer.snapshot(json!("invalid"));
                round.as_object_mut().unwrap().remove("result");
                round["results"] = match malformed {
                    4 => json!([{"key":"other","result":"invalid"}]),
                    5 => {
                        json!([{"key":"query","result":"invalid"},{"key":"query","result":"duplicate"}])
                    }
                    6 => json!([{"key":"query","result":"invalid","error":{"code":"forbidden"}}]),
                    7 => json!([]),
                    8 => json!([{"key":"query","error":{"code":"unknown"}}]),
                    _ => json!([{"key":"query","result":"invalid","page":{"range":""}}]),
                };
                vec![
                    format!(
                        "event: round\nid: {subscription}:1\ndata: {}\n\n",
                        json!({"subscription":subscription,"sequence":"1","round":round})
                    )
                    .into_bytes(),
                ]
            }
        };
        f.peer.enqueue(frames, true);
        let mut watch = hulladb::scope(runtime.clone(), value::watch())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if matches!(*watch.state(), QueryState::Error(_)) {
                    break;
                }
                watch.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), watch.changed())
                .await
                .unwrap()
                .is_err()
        );
        drop(watch);
        drop(runtime);
        let runtime = f.connect().await;
        let watch = hulladb::scope(runtime, value::watch()).await.unwrap();
        assert!(matches!(*watch.state(), QueryState::Loading));
    }
}

#[cfg(not(feature = "server"))]
#[hulladb::query(name = "client.erased")]
async fn erased(_: QueryContext<'_, ServerOnlyIdentity>) -> Result<String, Error> {
    unavailable_server_dependency::execute().await
}
#[cfg(not(feature = "server"))]
#[tokio::test]
async fn client_only_contract_omits_server_context_identity_and_handler_dependencies() {
    assert_eq!(erased::NAME, "client.erased");
    assert!(matches!(erased().await, Err(Error::InvalidInput(_))));
}

#[cfg(feature = "server")]
mod postgres {
    use super::*;
    use hulladb::{
        Authorization, Context, FunctionFuture, Invocation, ManagedSchema, ManagedStore, Registry,
        Table,
    };
    use hulladb_server::{
        AuthenticationError, HostOptions,
        managed::{
            AuthenticationFuture, Authenticator, CredentialHeaders, ManagedHost,
            Options as HostOptionsConfig,
        },
    };
    #[derive(Table)]
    #[table(name = "notes", key = "key")]
    struct Note {
        key: String,
        text: String,
    }
    struct Allow;
    impl Authorization<()> for Allow {
        fn authorize<'a>(
            &'a self,
            _: QueryContext<'a, ()>,
            _: Invocation,
            _: &'a Value,
        ) -> FunctionFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
    }
    struct Credentials(Mutex<Context<()>>);
    impl Authenticator<()> for Credentials {
        fn authenticate<'a>(
            &'a self,
            headers: &'a CredentialHeaders,
        ) -> AuthenticationFuture<'a, ()> {
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

                if bearer == "opaque" {
                    Ok(self.0.lock().unwrap().clone())
                } else {
                    Err(AuthenticationError::InvalidCredentials)
                }
            })
        }
    }
    #[hulladb::mutation(name = "notes.save")]
    async fn save(ctx: MutationContext<'_, ()>, text: String) -> Result<String, Error> {
        ctx.db
            .insert(NoteInsert {
                key: "first".into(),
                text: text.clone(),
            })
            .await?;
        Ok(text)
    }
    #[hulladb::query(name = "notes.read")]
    async fn read(ctx: QueryContext<'_, ()>) -> Result<String, Error> {
        Ok(ctx
            .db
            .get(hulladb::Id::<Note>::new("first".into()))
            .await?
            .map(|note| note.row.text)
            .unwrap_or_default())
    }
    #[tokio::test]
    #[ignore = "requires PostgreSQL; run make remote-functions-test"]
    async fn typed_remote_calls_sse_and_identity_pinning_cross_the_real_host() {
        let url = std::env::var("HULLADB_TEST_URL")
            .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into());
        let schema = ManagedSchema::new(
            format!("remote_{}", Uuid::now_v7().simple()),
            hulladb::schema!(Note).unwrap(),
        )
        .unwrap();
        let mut store = ManagedStore::connect(&url, schema.clone()).await.unwrap();
        store.initialize().await.unwrap();
        let context = Context {
            scope: Uuid::now_v7(),
            principal: Uuid::now_v7(),
            authorization_scope: Uuid::now_v7(),
            identity: (),
        };
        store.create_scope(context.scope).await.unwrap();
        let session = Session {
            scope: context.scope,
            principal: context.principal,
            authorization_scope: context.authorization_scope,
        };
        let credentials = Arc::new(Credentials(Mutex::new(context)));
        let mut registry = Registry::new();
        save::register(&mut registry, Allow).unwrap();
        read::register(&mut registry, Allow).unwrap();
        let host = ManagedHost::connect(
            &url,
            schema.clone(),
            registry,
            credentials.clone(),
            HostOptionsConfig {
                budgets: HostOptions {
                    poll_interval: Duration::from_millis(20),
                    credential_check_interval: Duration::from_millis(20),
                    operation_timeout: Duration::from_secs(2),
                    database_connections: 2,
                    max_inflight_requests: 16,
                    max_subscriptions: 4,
                    max_body_bytes: 65536,
                },
                cors_origins: vec![],
                principal_quotas: None,
            },
        )
        .await
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = Endpoint::local(listener.local_addr().unwrap()).unwrap();
        let router = host.router();
        let serving = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let local_path =
            std::env::temp_dir().join(format!("remote-host-{}.sqlite", Uuid::now_v7()));
        let local = RecoveryStore::open(&local_path).await.unwrap();
        let runtime = Arc::new(
            RemoteRuntime::connect(
                endpoint.clone(),
                "opaque",
                session,
                local.clone(),
                Options::default(),
            )
            .await
            .unwrap(),
        );
        let mut watch = hulladb::scope(runtime.clone(), read::watch())
            .await
            .unwrap();
        assert_eq!(wait_ready(&mut watch, true).await, "");
        assert_eq!(
            hulladb::scope(runtime.clone(), save("persisted".into()))
                .await
                .unwrap(),
            "persisted"
        );
        let receipt = local
            .completed(runtime.partition())
            .await
            .unwrap()
            .pop()
            .unwrap()
            .receipt;
        watch
            .wait_visible(&receipt, Duration::from_secs(2))
            .await
            .unwrap();
        assert!(
            matches!(&*watch.state(), QueryState::Ready{data, connected:true,..} if data=="persisted")
        );
        assert_eq!(
            hulladb::scope(runtime.clone(), read()).await.unwrap(),
            "persisted"
        );
        credentials.0.lock().unwrap().principal = Uuid::now_v7();
        assert!(matches!(
            hulladb::scope(runtime.clone(), save("wrong-owner".into())).await,
            Err(Error::Client(ClientFailure::Unauthorized))
        ));
        // The identity precondition rejects execution before a receipt is created.
        assert_eq!(local.completed(runtime.partition()).await.unwrap().len(), 1);
        drop(watch);
        runtime.retire().await.unwrap();
        serving.abort();
        drop(host);
        drop(store);
        let (db, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
            .await
            .unwrap();
        let connection = tokio::spawn(connection);
        db.batch_execute(&format!("DROP SCHEMA \"{}\" CASCADE", schema.namespace()))
            .await
            .unwrap();
        drop(db);
        connection.abort();
        drop(runtime);
        drop(local);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", local_path.display()));
        }
    }
}

#[tokio::test]
async fn retirement_during_an_inflight_ack_preserves_the_original_write() {
    let f = Fixture::new().await;
    let runtime = f.connect().await;
    f.peer.mode.store(2, Ordering::SeqCst);
    let call = tokio::spawn(hulladb::scope(runtime.clone(), write("retired".into())));
    f.peer.entered.notified().await;
    let id = runtime.pending().await.unwrap()[0].request_id;
    runtime.retire().await.unwrap();
    f.peer.release.notify_one();
    assert!(matches!(
        call.await.unwrap(),
        Err(Error::Client(ClientFailure::Closed))
    ));
    assert_eq!(runtime.pending().await.unwrap()[0].request_id, id);
    assert!(
        f.store()
            .await
            .completed(runtime.partition())
            .await
            .unwrap()
            .is_empty()
    );
    drop(runtime);
    let reopened = f.connect().await;
    assert_eq!(
        reopened
            .recover_pending(&[write::contract()])
            .await
            .unwrap()[0]
            .request_id,
        id
    );
}

#[tokio::test]
async fn pending_capacity_rejects_before_submission_and_incompatible_storage_is_preserved() {
    let f = Fixture::new().await;
    let mut options = f.options();
    options.max_pending_mutations = 1;
    options.recovery_attempts = 1;
    let runtime = Arc::new(
        RemoteRuntime::connect(
            f.endpoint.clone(),
            "secret",
            f.peer.session,
            f.store().await,
            options,
        )
        .await
        .unwrap(),
    );
    f.peer.mode.store(3, Ordering::SeqCst);
    assert!(matches!(
        hulladb::scope(runtime.clone(), write("first".into())).await,
        Err(Error::CommitOutcomeUnknown)
    ));
    assert!(matches!(
        hulladb::scope(runtime, write("second".into())).await,
        Err(Error::Client(ClientFailure::Capacity))
    ));
    assert_eq!(f.peer.attempts.lock().unwrap().len(), 1);
    for volatile in [":memory:", ""] {
        assert!(matches!(
            RecoveryStore::open(volatile).await,
            Err(Error::Client(ClientFailure::Storage))
        ));
    }
    let old = f.path.with_extension("old.sqlite");
    let db = rusqlite::Connection::open(&old).unwrap();
    db.execute_batch("CREATE TABLE preserved (value TEXT); INSERT INTO preserved VALUES ('keep');")
        .unwrap();
    assert!(matches!(
        RecoveryStore::open(&old).await,
        Err(Error::Client(ClientFailure::Storage))
    ));
    let preserved: String = db
        .query_row("SELECT value FROM preserved", [], |row| row.get(0))
        .unwrap();
    assert_eq!(preserved, "keep");
    drop(db);
    std::fs::remove_file(old).unwrap();
}

#[tokio::test]
async fn an_older_live_snapshot_cannot_certify_a_newer_cached_revision() {
    let f = Fixture::new().await;
    let runtime = f.connect().await;
    f.peer.revision.store(5, Ordering::SeqCst);
    f.peer.enqueue(
        vec![f.peer.frame(Uuid::now_v7(), 1, json!("cached-at-five"))],
        true,
    );
    let mut subscription = hulladb::scope(runtime.clone(), value::watch())
        .await
        .unwrap();
    wait_ready(&mut subscription, true).await;
    drop(subscription);
    drop(runtime);
    f.peer.revision.store(3, Ordering::SeqCst);
    let runtime = f.connect().await;
    f.peer.enqueue(
        vec![f.peer.frame(Uuid::now_v7(), 1, json!("older-at-three"))],
        true,
    );
    let mut subscription = hulladb::scope(runtime, value::watch()).await.unwrap();
    assert_eq!(wait_ready(&mut subscription, false).await, "cached-at-five");
    let fence = hulladb::Receipt {
        request_id: Uuid::now_v7(),
        source: f.peer.source(),
        revision: hulladb::Revision {
            scope: f.peer.session.scope,
            sequence: 5,
        },
        result: Value::Null,
    };
    assert!(
        subscription
            .wait_visible(&fence, Duration::from_millis(40))
            .await
            .is_err()
    );
    assert!(
        matches!(&*subscription.state(),QueryState::Ready{data,connected:false,..} if data=="cached-at-five")
    );
}

#[tokio::test]
async fn denied_restart_handshake_erases_cached_results_without_erasing_intents() {
    let f = Fixture::new().await;
    let mut options = f.options();
    options.recovery_attempts = 1;
    let runtime = Arc::new(
        RemoteRuntime::connect(
            f.endpoint.clone(),
            "secret",
            f.peer.session,
            f.store().await,
            options,
        )
        .await
        .unwrap(),
    );
    hulladb::scope(runtime.clone(), value()).await.unwrap();
    f.peer.mode.store(3, Ordering::SeqCst);
    assert!(
        hulladb::scope(runtime.clone(), write("pending".into()))
            .await
            .is_err()
    );
    let partition = runtime.partition().clone();
    drop(runtime);
    f.peer.deny.store(2, Ordering::SeqCst);
    assert!(matches!(
        RemoteRuntime::connect(
            f.endpoint.clone(),
            "secret",
            f.peer.session,
            f.store().await,
            options
        )
        .await,
        Err(Error::Forbidden)
    ));
    assert_eq!(f.store().await.pending(&partition).await.unwrap().len(), 1);
    f.peer.deny.store(0, Ordering::SeqCst);
    let runtime = f.connect().await;
    let subscription = hulladb::scope(runtime, value::watch()).await.unwrap();
    assert!(matches!(*subscription.state(), QueryState::Loading));
}

#[tokio::test]
async fn mutation_envelope_budget_is_checked_before_creating_a_durable_intent() {
    let f = Fixture::new().await;
    let mut options = f.options();
    options.transport.max_response_bytes = 400;
    let runtime = Arc::new(
        RemoteRuntime::connect(
            f.endpoint.clone(),
            "secret",
            f.peer.session,
            f.store().await,
            options,
        )
        .await
        .unwrap(),
    );
    assert!(matches!(
        hulladb::scope(runtime.clone(), write("x".repeat(280))).await,
        Err(Error::Client(ClientFailure::Budget))
    ));
    assert!(runtime.pending().await.unwrap().is_empty());
    assert!(f.peer.attempts.lock().unwrap().is_empty());
}

// At risk: treating an expired receipt like a missing one would resend committed work.
#[tokio::test]
async fn expired_recovery_retires_the_intent_and_never_resubmits() {
    let f = Fixture::new().await;
    f.peer.mode.store(2, Ordering::SeqCst);
    let runtime = f.connect().await;
    let call = tokio::spawn(hulladb::scope(runtime.clone(), write("committed".into())));
    f.peer.entered.notified().await;
    call.abort();
    let _ = call.await;
    let attempts = f.peer.attempts.lock().unwrap().len();
    f.peer.deny.store(3, Ordering::SeqCst);
    assert!(matches!(
        runtime.recover_pending(&[write::contract()]).await,
        Err(Error::ReceiptExpired)
    ));
    assert!(runtime.pending().await.unwrap().is_empty());
    drop(runtime);
    let reopened = f.connect().await;
    assert!(
        reopened
            .recover_pending(&[write::contract()])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(reopened.pending().await.unwrap().is_empty());
    assert_eq!(f.peer.attempts.lock().unwrap().len(), attempts);
    assert_eq!(f.peer.commits.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn renewal_only_changes_future_invocations_and_original_ticket_survives_restart() {
    let f = Fixture::new().await;
    f.peer.renewal_ms.store(10, Ordering::SeqCst);
    let runtime = f.connect().await;
    f.peer.mode.store(3, Ordering::SeqCst);
    assert!(matches!(
        hulladb::scope(runtime.clone(), write("original".into())).await,
        Err(Error::CommitOutcomeUnknown)
    ));
    let original = runtime.pending().await.unwrap()[0].request_id;
    let original_ticket = f.peer.tickets.lock().unwrap()[&original];
    f.peer.mode.store(0, Ordering::SeqCst);
    hulladb::scope(runtime.clone(), write("new".into()))
        .await
        .unwrap();
    assert!(f.peer.issued.load(Ordering::SeqCst) >= 2);
    assert_eq!(f.peer.tickets.lock().unwrap()[&original], original_ticket);
    drop(runtime);
    let reopened = f.connect().await;
    assert!(matches!(
        reopened.recover_pending(&[write::contract()]).await,
        Err(Error::CommitOutcomeUnknown)
    ));
    assert_eq!(
        f.peer
            .attempts
            .lock()
            .unwrap()
            .iter()
            .filter(|id| **id == original)
            .count(),
        1
    );
}

#[tokio::test]
async fn persisted_quota_failure_is_terminal_while_capacity_remains_recoverable() {
    let f = Fixture::new().await;
    let runtime = f.connect().await;
    f.peer.mode.store(5, Ordering::SeqCst);
    assert!(matches!(
        hulladb::scope(runtime.clone(), write("quota".into())).await,
        Err(Error::QuotaExceeded)
    ));
    assert!(runtime.pending().await.unwrap().is_empty());
    f.peer.mode.store(6, Ordering::SeqCst);
    assert!(matches!(
        hulladb::scope(runtime.clone(), write("capacity".into())).await,
        Err(Error::CommitOutcomeUnknown)
    ));
    assert_eq!(runtime.pending().await.unwrap().len(), 1);
    assert_eq!(f.peer.attempts.lock().unwrap().len(), 2);
}

// A dropped initial failure response must not strand a terminal invocation in the journal.
#[tokio::test]
async fn terminal_failure_lookup_retires_pending_once_without_resubmission() {
    let f = Fixture::new().await;
    let runtime = f.connect().await;
    f.peer.mode.store(3, Ordering::SeqCst);
    assert!(matches!(
        hulladb::scope(runtime.clone(), write("failed".into())).await,
        Err(Error::CommitOutcomeUnknown)
    ));
    assert_eq!(runtime.pending().await.unwrap().len(), 1);
    f.peer.deny.store(4, Ordering::SeqCst);
    assert!(matches!(
        runtime.recover_pending(&[write::contract()]).await,
        Err(Error::TerminalFailure(_))
    ));
    assert!(runtime.pending().await.unwrap().is_empty());
    drop(runtime);
    let reopened = f.connect().await;
    assert!(
        reopened
            .recover_pending(&[write::contract()])
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(f.peer.attempts.lock().unwrap().len(), 1);
}

// A dropped initial failure response must not strand a terminal invocation in the journal.
#[tokio::test]
async fn execution_limit_lookup_retires_pending_once_without_resubmission() {
    let f = Fixture::new().await;
    let runtime = f.connect().await;
    f.peer.mode.store(3, Ordering::SeqCst);
    assert!(matches!(
        hulladb::scope(runtime.clone(), write("failed".into())).await,
        Err(Error::CommitOutcomeUnknown)
    ));
    assert_eq!(runtime.pending().await.unwrap().len(), 1);
    f.peer.deny.store(5, Ordering::SeqCst);
    assert!(matches!(
        runtime.recover_pending(&[write::contract()]).await,
        Err(Error::TransactionDeadline)
    ));
    assert!(runtime.pending().await.unwrap().is_empty());
    drop(runtime);
    let reopened = f.connect().await;
    assert!(
        reopened
            .recover_pending(&[write::contract()])
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(f.peer.attempts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn forbidden_round_erases_cached_query_and_recovers_on_the_same_stream() {
    let f = Fixture::new().await;
    let runtime = f.connect().await;
    hulladb::scope(runtime.clone(), value()).await.unwrap();
    let subscription_id = Uuid::now_v7();
    let mut round = f.peer.snapshot(Value::Null);
    round.as_object_mut().unwrap().remove("result");
    round["results"] = json!([{"key":"query","error":{"code":"forbidden"}}]);
    let denied = format!(
        "event: round\nid: {subscription_id}:1\ndata: {}\n\n",
        json!({"subscription":subscription_id,"sequence":"1","round":round})
    )
    .into_bytes();
    // Gate the second frame at the HTTP boundary so the error state is observable.
    f.peer.scripts.lock().unwrap().push_back(Script {
        chunks: vec![denied, f.peer.frame(subscription_id, 2, json!("recovered"))],
        open: true,
        gate_after_first: true,
    });
    let mut subscription = hulladb::scope(runtime.clone(), value::watch())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if matches!(&*subscription.state(), QueryState::Error(error) if matches!(&**error, Error::Forbidden)) { break; }
            subscription.changed().await.unwrap();
        }
    }).await.unwrap();
    let db = rusqlite::Connection::open(&f.path).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM function_results", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    f.peer.release.notify_one();
    subscription.changed().await.unwrap();
    assert_eq!(wait_ready(&mut subscription, true).await, "recovered");
    assert_eq!(f.peer.streams.load(Ordering::SeqCst), 1);
    hulladb::scope(runtime.clone(), value()).await.unwrap();
    drop(subscription);
}

#[tokio::test]
async fn denied_mutation_ticket_renewal_retires_cached_state_but_preserves_recovery() {
    let f = Fixture::new().await;
    f.peer.renewal_ms.store(10, Ordering::SeqCst);
    let runtime = f.connect().await;
    hulladb::scope(runtime.clone(), value()).await.unwrap();
    f.peer.mode.store(3, Ordering::SeqCst);
    assert!(
        hulladb::scope(runtime.clone(), write("pending".into()))
            .await
            .is_err()
    );
    tokio::time::sleep(Duration::from_millis(15)).await;
    f.peer.deny.store(2, Ordering::SeqCst);
    assert!(matches!(
        hulladb::scope(runtime.clone(), write("denied".into())).await,
        Err(Error::Forbidden)
    ));
    assert!(*runtime.retirement().borrow());
    assert_eq!(
        f.store()
            .await
            .pending(runtime.partition())
            .await
            .unwrap()
            .len(),
        1
    );
    let db = rusqlite::Connection::open(&f.path).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM function_results", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    f.peer.deny.store(0, Ordering::SeqCst);
    assert!(matches!(
        hulladb::scope(runtime, value()).await,
        Err(Error::Client(ClientFailure::Closed))
    ));
}

#[tokio::test]
async fn query_cache_churn_is_bounded_without_evicting_durable_receipts() {
    let f = Fixture::new().await;
    let runtime = f.connect().await;
    hulladb::scope(runtime.clone(), write("delivered".into()))
        .await
        .unwrap();
    for i in 0..130 {
        runtime.query("notes.value", json!(i)).await.unwrap();
    }
    let db = rusqlite::Connection::open(&f.path).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM function_results", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        128
    );
    for i in 0..34 {
        runtime
            .query(
                "notes.value",
                json!(format!("{i}:{}", "x".repeat(512 * 1024))),
            )
            .await
            .unwrap();
    }
    let bytes: i64 = db.query_row("SELECT SUM(length(CAST(partition AS BLOB)) + length(CAST(query AS BLOB)) + length(CAST(snapshot AS BLOB))) FROM function_results", [], |row| row.get(0)).unwrap();
    assert!(bytes <= 16 * 1024 * 1024);
    assert_eq!(
        f.store()
            .await
            .completed(runtime.partition())
            .await
            .unwrap()
            .len(),
        1
    );
    drop(db);
    drop(runtime);
    let reopened = f.connect().await;
    assert_eq!(
        f.store()
            .await
            .completed(reopened.partition())
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        hulladb::scope(reopened, value()).await.unwrap(),
        "from-query"
    );
}
