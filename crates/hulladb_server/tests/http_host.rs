#[path = "../../hulladb_sync/tests/support/commands.rs"]
mod commands;

use axum::{
    body::{Body, Bytes},
    http::{Request, Response, StatusCode, header},
};
use commands::CommandFixture;
use http_body_util::{BodyExt, Full};
use hulladb_client::{ReplicaOptions, TaskReplica};
use hulladb_protocol::{
    ApiError, ApiErrorCode, Command, CommandRequest, Receipt, Session, TaskMessage, TaskQuery,
    TaskResult,
};
use hulladb_server::{
    AuthenticationError, AuthenticationFuture, Authenticator, HostOptions, TaskHost,
};
use hyper::{
    body::Incoming,
    client::conn::http1::{SendRequest, handshake},
};
use hyper_util::rt::TokioIo;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tower::ServiceExt;
use uuid::Uuid;

#[derive(Default)]
struct Credentials(Mutex<HashMap<String, Session>>);
impl Authenticator for Credentials {
    fn authenticate<'a>(&'a self, bearer: &'a str) -> AuthenticationFuture<'a> {
        Box::pin(async move {
            self.0
                .lock()
                .unwrap()
                .get(bearer)
                .copied()
                .ok_or(AuthenticationError::InvalidCredentials)
        })
    }
}
fn options() -> HostOptions {
    HostOptions {
        poll_interval: Duration::from_millis(40),
        credential_check_interval: Duration::from_millis(40),
        operation_timeout: Duration::from_secs(2),
        database_connections: 2,
        max_inflight_requests: 16,
        max_subscriptions: 8,
        max_body_bytes: 8192,
    }
}
fn query() -> TaskQuery {
    TaskQuery {
        completed: None,
        after: None,
        limit: 100,
    }
}
fn create(id: Uuid, title: &str) -> CommandRequest {
    CommandRequest {
        request_id: Uuid::now_v7(),
        command: Command::Create {
            id,
            title: title.into(),
            completed: false,
        },
    }
}

struct Fixture {
    f: CommandFixture,
    auth: Arc<Credentials>,
    host: TaskHost,
    server: JoinHandle<()>,
    address: SocketAddr,
    application: String,
}
impl Fixture {
    async fn start(options: HostOptions) -> Self {
        let f = CommandFixture::create(0).await.unwrap();
        let auth = Arc::new(Credentials::default());
        auth.0.lock().unwrap().insert("owner".into(), f.session);
        let application = format!("hulladb-http-{}", Uuid::now_v7());
        let mut config: tokio_postgres::Config = f.url.parse().unwrap();
        config.application_name(&application);
        let host = TaskHost::connect(config, auth.clone(), options)
            .await
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = host.router();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            f,
            auth,
            host,
            server,
            address,
            application,
        }
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        bearer: Option<&str>,
        payload: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let mut http = Http::connect(self.address).await;
        let response = http
            .request(method, path, bearer, serde_json::to_vec(&payload).unwrap())
            .await;
        let status = response.status();
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let bytes = tokio::time::timeout(Duration::from_secs(4), response.into_body().collect())
            .await
            .unwrap()
            .unwrap()
            .to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
    async fn subscribe(&self, bearer: &str) -> Events {
        let mut http = Http::connect(self.address).await;
        let response = http
            .request(
                "POST",
                "/tasks/subscriptions",
                Some(bearer),
                serde_json::to_vec(&query()).unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        Events {
            body: response.into_body(),
            buffer: Vec::new(),
            _http: http,
        }
    }
    async fn cleanup(self) {
        self.host.shutdown().await;
        self.server.abort();
        let _ = self.server.await;
        self.f.cleanup().await.unwrap();
    }
}
struct Http {
    sender: SendRequest<Full<Bytes>>,
    connection: JoinHandle<()>,
}
impl Http {
    async fn connect(address: SocketAddr) -> Self {
        let socket = TcpStream::connect(address).await.unwrap();
        let (sender, connection) = handshake(TokioIo::new(socket)).await.unwrap();
        let connection = tokio::spawn(async move {
            let _ = connection.await;
        });
        Self { sender, connection }
    }
    async fn request(
        &mut self,
        method: &str,
        path: &str,
        bearer: Option<&str>,
        bytes: Vec<u8>,
    ) -> Response<Incoming> {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "localhost")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(bearer) = bearer {
            request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
        }
        tokio::time::timeout(
            Duration::from_secs(4),
            self.sender
                .send_request(request.body(Full::new(Bytes::from(bytes))).unwrap()),
        )
        .await
        .unwrap()
        .unwrap()
    }
}
impl Drop for Http {
    fn drop(&mut self) {
        self.connection.abort();
    }
}
struct Events {
    body: Incoming,
    buffer: Vec<u8>,
    _http: Http,
}
impl Events {
    async fn next(&mut self) -> (String, serde_json::Value) {
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if let Some(end) = self.buffer.windows(2).position(|bytes| bytes == b"\n\n") {
                    let frame = String::from_utf8(self.buffer.drain(..end + 2).collect()).unwrap();
                    let event = frame.lines().find_map(|line| line.strip_prefix("event: "));
                    let data = frame.lines().find_map(|line| line.strip_prefix("data: "));
                    if let (Some(event), Some(data)) = (event, data) {
                        return (event.into(), serde_json::from_str(data).unwrap());
                    }
                    continue;
                }
                let frame = self
                    .body
                    .frame()
                    .await
                    .expect("stream ended before event")
                    .unwrap();
                if let Ok(bytes) = frame.into_data() {
                    self.buffer.extend_from_slice(&bytes);
                }
            }
        })
        .await
        .expect("no event before deadline")
    }
    async fn delivery(&mut self) -> TaskMessage {
        let (event, payload) = self.next().await;
        assert_eq!(event, "delivery", "{payload}");
        serde_json::from_value(payload).unwrap()
    }
}
fn assert_error(value: serde_json::Value, code: ApiErrorCode) {
    assert_eq!(
        serde_json::from_value::<ApiError>(value).unwrap().code,
        code
    );
}

#[tokio::test]
#[ignore = "requires local PostgreSQL and loopback sockets"]
async fn authentication_scope_validation_and_receipt_recovery_cross_real_http() {
    let mut f = Fixture::start(options()).await;
    let command = create(Uuid::now_v7(), "safe");
    for bearer in [None, Some("invalid")] {
        let (status, error) = f
            .request(
                "POST",
                "/tasks/commands",
                bearer,
                serde_json::to_value(&command).unwrap(),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_error(error, ApiErrorCode::Unauthorized);
    }
    let mut forged = serde_json::to_value(&command).unwrap();
    forged["session"] = serde_json::to_value(f.f.session).unwrap();
    let (status, error) = f
        .request("POST", "/tasks/commands", Some("owner"), forged)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_error(error, ApiErrorCode::InvalidRequest);
    let mut nested = serde_json::to_value(&command).unwrap();
    nested["command"]["Create"]["principal"] = serde_json::json!(Uuid::now_v7());
    assert_error(
        f.request("POST", "/tasks/commands", Some("owner"), nested)
            .await
            .1,
        ApiErrorCode::InvalidRequest,
    );
    assert!(
        f.f.store
            .list_tasks(f.f.session, None, None, 100)
            .await
            .unwrap()
            .tasks
            .is_empty()
    );
    let (status, result) = f
        .request(
            "POST",
            "/tasks/commands",
            Some("owner"),
            serde_json::to_value(&command).unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let committed: Receipt = serde_json::from_value(result.clone()).unwrap();
    assert_eq!(committed.request_id, command.request_id);
    assert_eq!(
        f.request(
            "POST",
            "/tasks/commands",
            Some("owner"),
            serde_json::to_value(&command).unwrap()
        )
        .await
        .1,
        result
    );
    assert_eq!(
        f.request(
            "GET",
            &format!("/tasks/receipts/{}", command.request_id),
            Some("owner"),
            serde_json::Value::Null
        )
        .await
        .1,
        result
    );
    let mut changed = command.clone();
    changed.command = Command::Create {
        id: Uuid::now_v7(),
        title: "different".into(),
        completed: false,
    };
    assert_error(
        f.request(
            "POST",
            "/tasks/commands",
            Some("owner"),
            serde_json::to_value(changed).unwrap(),
        )
        .await
        .1,
        ApiErrorCode::RequestIdReused,
    );
    let reader = Session {
        principal: Uuid::now_v7(),
        ..f.f.session
    };
    f.f.store
        .set_member(reader.workspace, reader.principal, Some(false))
        .await
        .unwrap();
    f.auth.0.lock().unwrap().insert("reader".into(), reader);
    assert_error(
        f.request(
            "POST",
            "/tasks/commands",
            Some("reader"),
            serde_json::to_value(create(Uuid::now_v7(), "denied")).unwrap(),
        )
        .await
        .1,
        ApiErrorCode::Forbidden,
    );
    assert_eq!(
        f.request(
            "GET",
            &format!("/tasks/receipts/{}", command.request_id),
            Some("reader"),
            serde_json::Value::Null
        )
        .await
        .1,
        serde_json::Value::Null
    );
    let (status, large) = f
        .request(
            "POST",
            "/tasks/commands",
            Some("owner"),
            serde_json::json!({"padding": "x".repeat(9000)}),
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_error(large, ApiErrorCode::InvalidRequest);
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL and loopback sockets"]
async fn periodic_polling_delivers_unhinted_commits_and_receipt_visibility_to_sqlite() {
    let mut f = Fixture::start(options()).await;
    let mut events = f.subscribe("owner").await;
    let initial = events.delivery().await;
    assert_eq!(initial.session, f.f.session);
    let mut client = TaskReplica::open(
        &f.f.client_path,
        f.f.session,
        ReplicaOptions {
            max_chunk_rows: 8,
            max_chunk_bytes: 32 * 1024,
            retained_deliveries_per_query: 2,
            busy_timeout: Duration::from_secs(1),
        },
    )
    .unwrap();
    client
        .register(query(), initial.cursor.subscription)
        .unwrap();
    client.apply(&initial).unwrap();
    let id = Uuid::now_v7();
    // Deliberately bypass the host wake-up, while using the supported managed write.
    let receipt =
        f.f.store
            .execute(
                f.f.session,
                Uuid::now_v7(),
                Command::Create {
                    id,
                    title: "line one\nline two é".into(),
                    completed: false,
                },
            )
            .await
            .unwrap();
    assert!(
        !client
            .is_visible(f.f.session, query(), receipt.visibility_fence())
            .unwrap()
    );
    let delivery = events.delivery().await;
    client.apply(&delivery).unwrap();
    assert!(
        client
            .is_visible(f.f.session, query(), receipt.visibility_fence())
            .unwrap()
    );
    assert_eq!(
        client.read(f.f.session, query()).unwrap().tasks[0].title,
        "line one\nline two é"
    );
    // An idle stream must not reserve a command pool connection.
    let request = CommandRequest {
        request_id: Uuid::now_v7(),
        command: Command::Delete {
            id,
            expected_version: 1,
        },
    };
    let (status, result) = f
        .request(
            "POST",
            "/tasks/commands",
            Some("owner"),
            serde_json::to_value(request).unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let receipt: Receipt = serde_json::from_value(result).unwrap();
    client.apply(&events.delivery().await).unwrap();
    assert!(
        client
            .is_visible(f.f.session, query(), receipt.visibility_fence())
            .unwrap()
    );
    assert!(client.read(f.f.session, query()).unwrap().tasks.is_empty());
    drop(client);
    drop(events);
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL and loopback sockets"]
async fn credential_changes_close_idle_streams_without_rebinding_their_scope() {
    let mut f = Fixture::start(options()).await;
    let mut events = f.subscribe("owner").await;
    events.delivery().await;
    let other = f.f.store.create_workspace(Uuid::now_v7()).await.unwrap();
    f.auth.0.lock().unwrap().insert("owner".into(), other);
    let (event, error) = events.next().await;
    assert_eq!(event, "error");
    assert_error(error, ApiErrorCode::Unauthorized);
    let mut fresh = f.subscribe("owner").await;
    assert_eq!(fresh.delivery().await.session, other);
    f.auth.0.lock().unwrap().remove("owner");
    let (event, error) = fresh.next().await;
    assert_eq!(event, "error");
    assert_error(error, ApiErrorCode::Unauthorized);
    drop(events);
    drop(fresh);
    f.f.store
        .delete_fixture_workspace(other.workspace)
        .await
        .unwrap();
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL and loopback sockets"]
async fn membership_revocation_prevents_queued_snapshot_delivery() {
    let mut f = Fixture::start(options()).await;
    f.f.store
        .execute(
            f.f.session,
            Uuid::now_v7(),
            Command::Create {
                id: Uuid::now_v7(),
                title: "private".into(),
                completed: false,
            },
        )
        .await
        .unwrap();
    // In-process Body intentionally remains unpolled until after revocation;
    // a real socket would be allowed to release bytes before we revoke access.
    let response = f
        .host
        .router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tasks/subscriptions")
                .header(header::AUTHORIZATION, "Bearer owner")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&query()).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    f.f.store
        .set_member(f.f.session.workspace, f.f.session.principal, None)
        .await
        .unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let output = std::str::from_utf8(&bytes).unwrap();
    assert!(output.contains("Revoked"), "{output}");
    assert!(!output.contains("private"));
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn dropping_slow_streams_releases_capacity_and_shutdown_closes_idle_streams() {
    let f = Fixture::start(HostOptions {
        max_subscriptions: 1,
        ..options()
    })
    .await;
    let router = f.host.router();
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/tasks/subscriptions")
            .header(header::AUTHORIZATION, "Bearer owner")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&query()).unwrap()))
            .unwrap()
    };
    let first = router.clone().oneshot(request()).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let second = router.clone().oneshot(request()).await.unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_error(
        serde_json::from_slice(&second.into_body().collect().await.unwrap().to_bytes()).unwrap(),
        ApiErrorCode::Capacity,
    );
    drop(first);
    let replacement = router.clone().oneshot(request()).await.unwrap();
    assert_eq!(replacement.status(), StatusCode::OK);
    f.host.stop();
    let bytes = tokio::time::timeout(Duration::from_secs(1), replacement.into_body().collect())
        .await
        .unwrap()
        .unwrap()
        .to_bytes();
    assert!(std::str::from_utf8(&bytes).unwrap().contains("unavailable"));
    assert_eq!(
        router.clone().oneshot(request()).await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    // Keep the router alive across shutdown and verify owned idle connections
    // are closed rather than living as long as the route's state clones.
    f.host.shutdown().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let active: i64 =
            f.f.db
                .query_one(
                    "SELECT count(*) FROM pg_stat_activity WHERE application_name=$1",
                    &[&f.application],
                )
                .await
                .unwrap()
                .get(0);
        if active == 0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "host retained connections after shutdown"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    drop(router);
    f.server.abort();
    let _ = f.server.await;
    f.f.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local PostgreSQL and loopback sockets"]
async fn database_disconnect_closes_old_stream_and_recovers_with_a_fresh_snapshot() {
    let mut f = Fixture::start(options()).await;
    let mut events = f.subscribe("owner").await;
    let initial = events.delivery().await;
    // Terminate only this host's owned connections; never the shared database.
    let killed = f.f.db.query("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name=$1 AND pid<>pg_backend_pid()", &[&f.application]).await.unwrap();
    assert_eq!(killed.len(), options().database_connections + 1);
    let (event, error) = events.next().await;
    assert_eq!(event, "error");
    assert_error(error, ApiErrorCode::Unavailable);
    f.f.store
        .execute(
            f.f.session,
            Uuid::now_v7(),
            Command::Create {
                id: Uuid::now_v7(),
                title: "during reconnect".into(),
                completed: false,
            },
        )
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    loop {
        let (status, _) = f
            .request(
                "GET",
                &format!("/tasks/receipts/{}", Uuid::now_v7()),
                Some("owner"),
                serde_json::Value::Null,
            )
            .await;
        if status == StatusCode::OK {
            break;
        }
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            tokio::time::Instant::now() < deadline,
            "host did not reconnect"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut fresh = f.subscribe("owner").await;
    let delivery = fresh.delivery().await;
    assert_ne!(delivery.cursor.subscription, initial.cursor.subscription);
    let TaskResult::Snapshot { tasks } = delivery.result else {
        panic!("expected recovered snapshot")
    };
    assert_eq!(tasks[0].title, "during reconnect");
    // The pool replaces terminated command connections as well as the poll worker.
    assert_eq!(
        f.request(
            "POST",
            "/tasks/commands",
            Some("owner"),
            serde_json::to_value(create(Uuid::now_v7(), "pooled reconnect")).unwrap()
        )
        .await
        .0,
        StatusCode::OK
    );
    drop(events);
    drop(fresh);
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL and loopback sockets"]
async fn timed_out_mutations_require_receipt_recovery_and_release_request_admission() {
    let mut f = Fixture::start(HostOptions {
        operation_timeout: Duration::from_millis(250),
        max_inflight_requests: 1,
        ..options()
    })
    .await;
    let tx = f.f.db.transaction().await.unwrap();
    tx.batch_execute("LOCK TABLE _hulladb_task_probe.receipts IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();
    let request = create(Uuid::now_v7(), "recover safely");
    let mut http = Http::connect(f.address).await;
    let bytes = serde_json::to_vec(&request).unwrap();
    let pending = tokio::spawn(async move {
        let response = http
            .request("POST", "/tasks/commands", Some("owner"), bytes)
            .await;
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice::<ApiError>(&bytes).unwrap())
    });
    // Wait until the actual mutation reaches the database rather than relying
    // on a sleep to decide whether the admission permit is occupied.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
    loop {
        let blocked: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM pg_locks l JOIN pg_stat_activity a ON a.pid=l.pid WHERE relation='_hulladb_task_probe.receipts'::regclass AND NOT granted AND a.application_name=$1)", &[&f.application]).await.unwrap().get(0);
        if blocked {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "request did not reach blocked read"
        );
        tokio::task::yield_now().await;
    }
    let mut rejected = Http::connect(f.address).await;
    let second = rejected
        .request(
            "GET",
            &format!("/tasks/receipts/{}", request.request_id),
            Some("owner"),
            b"null".to_vec(),
        )
        .await;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    let bytes = second.into_body().collect().await.unwrap().to_bytes();
    assert_error(
        serde_json::from_slice(&bytes).unwrap(),
        ApiErrorCode::Capacity,
    );
    let (status, error) = pending.await.unwrap();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error.code, ApiErrorCode::OutcomeUnknown);
    tx.commit().await.unwrap();
    let (status, recovered) = f
        .request(
            "GET",
            &format!("/tasks/receipts/{}", request.request_id),
            Some("owner"),
            serde_json::Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(recovered.is_null(), "blocked operation must roll back");
    let (status, receipt) = f
        .request(
            "POST",
            "/tasks/commands",
            Some("owner"),
            serde_json::to_value(&request).unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        f.request(
            "POST",
            "/tasks/commands",
            Some("owner"),
            serde_json::to_value(&request).unwrap()
        )
        .await
        .1,
        receipt
    );
    assert_eq!(
        f.f.store
            .list_tasks(f.f.session, None, None, 100)
            .await
            .unwrap()
            .tasks
            .len(),
        1
    );
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL"]
async fn unread_old_epochs_keep_capacity_and_cannot_release_stale_snapshots_after_reconnect() {
    let mut f = Fixture::start(HostOptions {
        max_subscriptions: 1,
        ..options()
    })
    .await;
    let id = Uuid::now_v7();
    f.f.store
        .execute(
            f.f.session,
            Uuid::now_v7(),
            Command::Create {
                id,
                title: "stale private result".into(),
                completed: false,
            },
        )
        .await
        .unwrap();
    let router = f.host.router();
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/tasks/subscriptions")
            .header(header::AUTHORIZATION, "Bearer owner")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&query()).unwrap()))
            .unwrap()
    };
    let old = router.clone().oneshot(request()).await.unwrap();
    assert_eq!(old.status(), StatusCode::OK);
    let killed = f.f.db.query("SELECT pid,pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name=$1 AND pid<>pg_backend_pid()", &[&f.application]).await.unwrap();
    let killed_pids = killed
        .iter()
        .map(|row| row.get::<_, i32>(0))
        .collect::<Vec<_>>();
    f.f.store
        .execute(
            f.f.session,
            Uuid::now_v7(),
            Command::Delete {
                id,
                expected_version: 1,
            },
        )
        .await
        .unwrap();
    // No HTTP body has been polled, so only the scheduler can create this new
    // application-named connection. Wait for reconnection before touching the body.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    loop {
        let active: bool =
            f.f.db
                .query_one(
                    "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name=$1 AND NOT(pid=ANY($2)))",
                    &[&f.application, &killed_pids],
                )
                .await
                .unwrap()
                .get(0);
        if active {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "poll worker did not reconnect"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    loop {
        let (status, _) = f
            .request(
                "GET",
                &format!("/tasks/receipts/{}", Uuid::now_v7()),
                Some("owner"),
                serde_json::Value::Null,
            )
            .await;
        if status == StatusCode::OK {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "host did not become ready"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let denied = router.clone().oneshot(request()).await.unwrap();
    assert_eq!(denied.status(), StatusCode::TOO_MANY_REQUESTS);
    let output = tokio::time::timeout(Duration::from_secs(1), old.into_body().collect())
        .await
        .unwrap()
        .unwrap()
        .to_bytes();
    let output = std::str::from_utf8(&output).unwrap();
    assert!(output.contains("unavailable"), "{output}");
    assert!(
        !output.contains("stale private result"),
        "old epoch leaked queued rows"
    );
    let new = router.oneshot(request()).await.unwrap();
    assert_eq!(new.status(), StatusCode::OK);
    drop(new);
    f.cleanup().await;
}
