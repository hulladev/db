//! Development HTTP/SSE host. Authentication remains in the trusted parent
//! application; declarations execute in the restricted JavaScript worker.
use hulladb_postgres::managed::{
    Authorization, Context, FunctionFuture, Invocation, ManagedStore, MutationConcurrency,
    QueryContext, Registry, RetryOptions,
    typescript::{TypescriptFunction, Worker, schema_from_descriptor},
};
use hulladb_server::{
    AuthenticationError, HostOptions,
    managed::{AuthenticationFuture, Authenticator, CredentialHeaders, ManagedHost, Options},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Stdin, Stdout},
    sync::{Mutex as AsyncMutex, oneshot, watch},
};
use uuid::Uuid;

const MAX_FRAME: u64 = 66 * 1024 * 1024;
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
type Failure = &'static str;
type Reply = oneshot::Sender<Result<Option<VerifiedContext>, AuthenticationError>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerConfig {
    executable: PathBuf,
    entrypoint: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Init {
    url: String,
    namespace: String,
    schema: Value,
    manifest: String,
    worker: WorkerConfig,
    host: String,
    port: u16,
    origins: Vec<String>,
    #[serde(default)]
    development: bool,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Init(Init),
    Authenticated {
        id: u64,
        context: Option<VerifiedContext>,
        #[serde(default)]
        unavailable: bool,
    },
    Shutdown,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VerifiedContext {
    principal: Uuid,
    authorization_scope: Uuid,
    identity: Value,
}

struct Output(AsyncMutex<Stdout>);
impl Output {
    async fn send(&self, frame: Value) -> Result<(), Failure> {
        let mut bytes = serde_json::to_vec(&frame).map_err(|_| "invalid service response")?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_FRAME {
            return Err("service response exceeds limit");
        }
        tokio::time::timeout(AUTH_TIMEOUT, async {
            let mut output = self.0.lock().await;
            output.write_all(&bytes).await?;
            output.flush().await
        })
        .await
        .map_err(|_| "service response timed out")?
        .map_err(|_| "service parent disconnected")
    }
}
async fn receive(input: &mut BufReader<Stdin>) -> Result<Option<Command>, Failure> {
    let mut bytes = Vec::new();
    let count = input
        .take(MAX_FRAME + 1)
        .read_until(b'\n', &mut bytes)
        .await
        .map_err(|_| "service input failed")?;
    if count == 0 {
        return Ok(None);
    }
    if count as u64 > MAX_FRAME || !bytes.ends_with(b"\n") {
        return Err("service command exceeds limit or is incomplete");
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| "invalid service command")
}

struct ParentAuth {
    output: Arc<Output>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, Reply>>,
    stopped: watch::Sender<bool>,
}
impl ParentAuth {
    fn stop(&self) {
        self.stopped.send_replace(true);
        self.pending.lock().expect("auth lock poisoned").clear();
    }
}
// Requests can be cancelled by the host while the verifier is running. Remove
// their channels on future drop as well as ordinary success, timeout and EOF.
struct Pending<'a> {
    parent: &'a ParentAuth,
    id: u64,
}
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.parent
            .pending
            .lock()
            .expect("auth lock poisoned")
            .remove(&self.id);
    }
}
impl Authenticator<Value> for ParentAuth {
    fn authenticate<'a>(
        &'a self,
        headers: &'a CredentialHeaders,
    ) -> AuthenticationFuture<'a, Value> {
        Box::pin(async move {
            if *self.stopped.borrow() {
                return Err(AuthenticationError::Unavailable);
            }
            let headers: Vec<_> = headers
                .iter()
                .map(|(name, value)| {
                    value
                        .to_str()
                        .map(|value| (name.as_str(), value))
                        .map_err(|_| AuthenticationError::InvalidCredentials)
                })
                .collect::<Result<_, _>>()?;
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let (sender, receiver) = oneshot::channel();
            {
                let mut pending = self.pending.lock().expect("auth lock poisoned");
                if *self.stopped.borrow() || pending.len() >= 256 {
                    return Err(AuthenticationError::Unavailable);
                }
                pending.insert(id, sender);
            }
            let _pending = Pending { parent: self, id };
            let verified = tokio::time::timeout(AUTH_TIMEOUT, async {
                self.output
                    .send(json!({"type":"authenticate", "id":id, "headers":headers}))
                    .await
                    .map_err(|_| AuthenticationError::Unavailable)?;
                receiver
                    .await
                    .map_err(|_| AuthenticationError::Unavailable)??
                    .ok_or(AuthenticationError::InvalidCredentials)
            })
            .await
            .map_err(|_| AuthenticationError::Unavailable)??;
            Ok(Context {
                scope: Uuid::nil(),
                principal: verified.principal,
                authorization_scope: verified.authorization_scope,
                identity: verified.identity,
            })
        })
    }
}

// Handler/extension guards authorize a declaration. Confirmation delivers the
// original authorized result to the same verified principal/context.
struct HandlerPolicy;
impl Authorization<Value> for HandlerPolicy {
    fn authorize<'a>(
        &'a self,
        _: QueryContext<'a, Value>,
        _: Invocation,
        _: &'a Value,
    ) -> FunctionFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

async fn start(
    init: Init,
    auth: Arc<ParentAuth>,
) -> Result<(ManagedHost<Value>, tokio::net::TcpListener), Failure> {
    if !init.worker.executable.is_absolute() || !init.worker.entrypoint.is_absolute() {
        return Err("worker paths must be absolute");
    }
    if !init.worker.executable.is_file() || !init.worker.entrypoint.is_file() {
        return Err(
            "restricted JavaScript engine or generated bundle is missing; build hulladb-js and regenerate",
        );
    }
    let schema = schema_from_descriptor(&init.namespace, init.schema)
        .map_err(|_| "invalid database schema")?;
    // Verify the default application scope too; startup never creates storage.
    let mut store = ManagedStore::connect(&init.url, schema.clone())
        .await
        .map_err(|_| "database connection failed")?;
    store
        .verify()
        .await
        .map_err(|_| "database schema verification failed; run init first")?;
    store
        .managed_head(&Context {
            scope: Uuid::nil(),
            principal: Uuid::nil(),
            authorization_scope: Uuid::nil(),
            identity: Value::Null,
        })
        .await
        .map_err(|_| "database application scope is missing; run init first")?;
    drop(store);
    let mut worker = Worker::new(
        init.worker.executable,
        vec![init.worker.entrypoint.into_os_string()],
    );
    worker.max_frame_bytes = MAX_FRAME as usize;
    worker.development = init.development;
    let worker = Arc::new(worker);
    let manifest: Value =
        serde_json::from_str(&init.manifest).map_err(|_| "invalid generated function manifest")?;
    let functions = manifest
        .get("functions")
        .and_then(Value::as_object)
        .ok_or("invalid generated function manifest")?;
    let mut registry = Registry::new();
    let mut payload_bytes = 16 * 1024 * 1024;
    let mut execution_ms = 5000u64;
    for name in functions.keys() {
        let function = TypescriptFunction::from_manifest(worker.clone(), name, &init.manifest)
            .map_err(|_| "invalid generated function declaration")?;
        let limits = function.limits();
        payload_bytes = payload_bytes
            .max(limits.input_bytes)
            .max(limits.output_bytes);
        execution_ms = execution_ms.max(limits.execution_ms);
        function
            .register_with_concurrency(
                &mut registry,
                HandlerPolicy,
                MutationConcurrency::Serializable(RetryOptions {
                    max_attempts: 3,
                    timeout: Duration::from_secs(60),
                    ..RetryOptions::default()
                }),
            )
            .map_err(|_| "function registration failed")?;
    }
    let host = ManagedHost::connect(
        &init.url,
        schema,
        registry,
        auth,
        Options {
            budgets: HostOptions {
                poll_interval: Duration::from_millis(100),
                credential_check_interval: Duration::from_secs(30),
                operation_timeout: Duration::from_millis((execution_ms + 5000).max(15_000)),
                database_connections: 4,
                max_inflight_requests: 64,
                max_subscriptions: 128,
                max_body_bytes: payload_bytes + 2 * 1024 * 1024,
            },
            cors_origins: init.origins,
            principal_quotas: None,
        },
    )
    .await
    .map_err(|_| "browser service initialization failed")?;
    let listener = tokio::net::TcpListener::bind((init.host.as_str(), init.port))
        .await
        .map_err(|_| "browser service address is unavailable")?;
    Ok((host, listener))
}

async fn run(output: Arc<Output>) -> Result<(), Failure> {
    let mut input = BufReader::new(tokio::io::stdin());
    let Some(Command::Init(init)) =
        tokio::time::timeout(Duration::from_secs(30), receive(&mut input))
            .await
            .map_err(|_| "service initialization timed out")??
    else {
        return Err("service init must be first");
    };
    let (stopped, mut stop) = watch::channel(false);
    let auth = Arc::new(ParentAuth {
        output: output.clone(),
        next_id: AtomicU64::new(1),
        pending: Mutex::new(HashMap::new()),
        stopped,
    });
    let reader_auth = auth.clone();
    let reader = tokio::spawn(async move {
        let result = loop {
            match receive(&mut input).await {
                Ok(Some(Command::Authenticated {
                    id,
                    context,
                    unavailable,
                })) => {
                    if let Some(reply) = reader_auth
                        .pending
                        .lock()
                        .expect("auth lock poisoned")
                        .remove(&id)
                    {
                        let _ = reply.send(if unavailable {
                            Err(AuthenticationError::Unavailable)
                        } else {
                            Ok(context)
                        });
                    }
                }
                Ok(None | Some(Command::Shutdown)) => break Ok(()),
                Ok(Some(Command::Init(_))) => break Err("service init may only be sent once"),
                Err(error) => break Err(error),
            }
        };
        reader_auth.stop();
        result
    });
    let outcome = async {
        let (host, listener) = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(30), start(init, auth.clone())) => result.map_err(|_| "service startup timed out")??,
            _ = stop.changed() => return Ok(()),
        };
        if *stop.borrow() { return Ok(()); }
        output.send(json!({"type":"ready", "url":format!("http://{}", listener.local_addr().map_err(|_| "service listener failed")?)})).await?;
        let host = Arc::new(host);
        let shutdown_host = host.clone();
        let mut shutdown_signal = stop.clone();
        let shutdown = async move {
            if !*shutdown_signal.borrow() { let _ = shutdown_signal.changed().await; }
            shutdown_host.stop();
        };
        let server = axum::serve(listener, host.router()).with_graceful_shutdown(shutdown);
        let mut server = tokio::spawn(async move { server.await });
        tokio::select! {
            result = &mut server => result.map_err(|_| "browser service task failed")?.map_err(|_| "browser service stopped unexpectedly"),
            _ = async { if !*stop.borrow() { let _ = stop.changed().await; } } => {
                host.stop();
                // Existing SSE streams retire immediately. Bound shutdown even
                // when an incomplete HTTP request or slow peer remains open.
                if tokio::time::timeout(Duration::from_secs(5), &mut server).await.is_err() {
                    server.abort();
                    let _ = server.await;
                }
                Ok(())
            }
        }
    }.await;
    let reader_stopped = *auth.stopped.borrow();
    auth.stop();
    let reader_result = if reader_stopped || reader.is_finished() {
        reader.await.map_err(|_| "service input task failed")?
    } else {
        reader.abort();
        let _ = reader.await;
        Ok(())
    };
    outcome.and(reader_result)
}

#[tokio::main]
async fn main() {
    let output = Arc::new(Output(AsyncMutex::new(tokio::io::stdout())));
    if let Err(message) = run(output.clone()).await {
        let _ = output
            .send(json!({"type":"error", "message":message}))
            .await;
        std::process::exit(1);
    }
}
