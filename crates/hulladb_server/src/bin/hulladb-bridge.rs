//! Private local stdio transport. Never listens on a network interface.
use hulladb_postgres::managed::{
    Context, Error, ManagedStore,
    typescript::{InvocationLimits, execute_direct_operation, schema_from_descriptor},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Stdin, Stdout},
    sync::Mutex,
};
use uuid::Uuid;
const MAX_FRAME: u64 = 66 * 1024 * 1024;
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Init {
        url: String,
        namespace: String,
        schema: Value,
        #[serde(default)]
        scope: Uuid,
        #[serde(default)]
        initialize: bool,
        #[serde(default)]
        limits: InvocationLimits,
        #[serde(default)]
        development: bool,
    },
    Begin {
        mode: Mode,
    },
    Operation {
        id: u64,
        operation: Value,
    },
    Ping,
    Commit,
    Rollback,
}
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Read,
    Write,
}
struct Io {
    input: BufReader<Stdin>,
    output: Stdout,
    operation: Option<u64>,
    expired: bool,
    frame_bytes: usize,
}
impl Io {
    async fn receive(&mut self) -> Result<Option<Command>, Error> {
        let mut bytes = Vec::new();
        let count = (&mut self.input)
            .take((self.frame_bytes + 1) as u64)
            .read_until(b'\n', &mut bytes)
            .await
            .map_err(|_| Error::FunctionExecution)?;
        if count == 0 {
            return Ok(None);
        }
        if count > self.frame_bytes || !bytes.ends_with(b"\n") {
            return Err(Error::InvalidInput(
                "database protocol frame exceeds limit or is incomplete".into(),
            ));
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| Error::InvalidInput("invalid database protocol command".into()))
    }
    async fn send(&mut self, value: Value) -> Result<(), Error> {
        let mut bytes = serde_json::to_vec(&value).map_err(|_| Error::FunctionExecution)?;
        bytes.push(b'\n');
        if bytes.len() > self.frame_bytes {
            return Err(Error::InvalidInput(
                "database result exceeds the transport budget".into(),
            ));
        }
        self.output
            .write_all(&bytes)
            .await
            .map_err(|_| Error::FunctionExecution)?;
        self.output
            .flush()
            .await
            .map_err(|_| Error::FunctionExecution)
    }
}
fn failure(error: &Error, id: Option<u64>) -> Value {
    let (code, message) = match error {
        Error::InvalidInput(message) => ("invalid_input", message.as_str()),
        Error::RowExpectationFailed => (
            "row_expectation_failed",
            "write affected an unexpected number of rows",
        ),
        Error::Validation(_) => (
            "validation_failed",
            "database value failed schema validation",
        ),
        Error::Forbidden => ("read_only", "writes are unavailable in a read transaction"),
        Error::Conflict => (
            "constraint_violation",
            "database uniqueness or row constraint conflict",
        ),
        Error::ReferenceViolation => (
            "reference_violation",
            "database reference constraint failed",
        ),
        Error::NotFound => ("not_found", "database row or scope does not exist"),
        Error::SchemaMismatch => (
            "schema_mismatch",
            "installed schema differs; recreate development fixtures explicitly",
        ),
        Error::TransactionDeadline => (
            "execution_limit",
            "Database invocation exceeded its execution budget",
        ),
        Error::QuotaExceeded => ("quota_exceeded", "database scope quota exceeded"),
        Error::CommitOutcomeUnknown => (
            "commit_outcome_unknown",
            "commit outcome could not be confirmed; do not replay automatically",
        ),
        Error::Database(error)
            if matches!(
                error.code(),
                Some(
                    &tokio_postgres::error::SqlState::T_R_SERIALIZATION_FAILURE
                        | &tokio_postgres::error::SqlState::T_R_DEADLOCK_DETECTED
                )
            ) =>
        {
            (
                "serialization_conflict",
                "database serialization conflict; transaction was rolled back",
            )
        }
        _ => ("database_failed", "database operation failed"),
    };
    // A driver transport failure also makes rollback acknowledgement uncertain.
    let rolled_back = !matches!(
        error,
        Error::CommitOutcomeUnknown | Error::Database(_) | Error::ConnectionDeadline
    ) || matches!(error, Error::Database(error) if error.as_db_error().is_some());
    json!({"type":"error", "id":id, "error":{"code":code,"message":message}, "rolledBack":rolled_back})
}
async fn run() -> Result<(), Error> {
    let io = Arc::new(Mutex::new(Io {
        input: BufReader::new(tokio::io::stdin()),
        output: tokio::io::stdout(),
        operation: None,
        expired: false,
        frame_bytes: MAX_FRAME as usize,
    }));
    let Some(Command::Init {
        url,
        namespace,
        schema,
        scope,
        initialize,
        limits,
        development,
    }) = io.lock().await.receive().await?
    else {
        return Err(Error::InvalidInput("database init must be first".into()));
    };
    let limits = limits.validate()?;
    io.lock().await.frame_bytes = limits.frame_bytes();
    // Enforced by PostgreSQL even when the JavaScript host or native process is
    // frozen. Callback awaits cannot retain an idle transaction indefinitely.
    let mut config: tokio_postgres::Config = url.parse()?;
    let options = format!(
        "{} -c idle_in_transaction_session_timeout={} -c idle_session_timeout=60000",
        config.get_options().unwrap_or(""),
        limits.execution_ms
    );
    config.options(&options);
    let mut store =
        ManagedStore::connect_config(config, schema_from_descriptor(&namespace, schema)?).await?;
    if initialize {
        store.initialize().await?;
    } else {
        store.verify().await?;
    }
    let context = Context {
        scope,
        principal: Uuid::nil(),
        authorization_scope: Uuid::nil(),
        identity: (),
    };
    match store.managed_head(&context).await {
        Ok(_) => {}
        Err(Error::NotFound) if initialize => {
            match store.create_scope(scope).await {
                Ok(()) => {}
                // A concurrent explicit initializer may have created the scope.
                Err(Error::Conflict) => {
                    store.managed_head(&context).await?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    }
    io.lock().await.send(json!({"type":"ready"})).await?;
    loop {
        // Idle native processes also retire if the JavaScript event loop stops.
        let received = tokio::time::timeout(Duration::from_secs(30), async {
            io.lock().await.receive().await
        })
        .await;
        let Some(command) = (match received {
            Ok(command) => command?,
            Err(_) => return Ok(()),
        }) else {
            return Ok(());
        };
        if matches!(command, Command::Ping) {
            store.managed_head(&context).await?;
            io.lock().await.send(json!({"type":"pong"})).await?;
            continue;
        }
        let Command::Begin { mode } = command else {
            return Err(Error::InvalidInput(
                "begin is required outside a transaction".into(),
            ));
        };
        let callback_io = io.clone();
        io.lock().await.operation = None;
        let result = store
            .direct_transaction(scope, matches!(mode, Mode::Write), move |read, write| {
                Box::pin(async move {
                    let mut io = callback_io.lock().await;
                    read.transaction_limits(limits.execution_ms).await?;
                    read.set_read_limits(limits.read_rows,limits.read_bytes)?;
                    io.send(json!({"type":"begun"})).await?;
                    let started = tokio::time::Instant::now();
                    let mut operations = 0usize;
                    loop {
                        io.operation = None;
                        let command =
                            match tokio::time::timeout_at(started + Duration::from_millis(limits.execution_ms), io.receive()).await
                            {
                                Ok(command) => command?,
                                Err(_) => {
                                    io.expired = true;
                                    return Err(Error::InvalidInput(
                                        "transaction callback exceeded the 30-second idle limit"
                                            .into(),
                                    ));
                                }
                            };
                        match command {
                            Some(Command::Operation { id, operation }) => {
                                operations += 1;
                                if operations > limits.operations { return Err(Error::TransactionDeadline); }
                                io.operation = Some(id);
                                let result =
                                    execute_direct_operation(operation, read, write.as_ref())
                                        .await?;
                                io.send(json!({"type":"operation_result","id":id,"result":result}))
                                    .await?;
                            }
                            Some(Command::Commit) => {
                                if development {
                                    let (read_rows,read_bytes) = read.read_usage()?;
                                    for (name,used,maximum) in [("executionMs",started.elapsed().as_millis() as usize,limits.execution_ms as usize),("operations",operations,limits.operations),("readRows",read_rows,limits.read_rows),("readBytes",read_bytes,limits.read_bytes)] {
                                        if used.saturating_mul(5) >= maximum.saturating_mul(4) { eprintln!("[hulla/db] direct transaction: {name} {used}/{maximum}"); }
                                    }
                                }
                                return Ok((false, true));
                            },
                            Some(Command::Rollback) => return Ok((false, false)),
                            None => return Ok((true, false)),
                            _ => {
                                return Err(Error::InvalidInput(
                                    "unexpected transaction command".into(),
                                ));
                            }
                        }
                    }
                })
            })
            .await;
        match result {
            Ok((true, _)) => return Ok(()),
            Ok((false, committed)) => io
                .lock()
                .await
                .send(json!({"type":if committed { "committed" } else { "rolled_back" }}))
                .await
                .map_err(|error| {
                    if committed {
                        Error::CommitOutcomeUnknown
                    } else {
                        error
                    }
                })?,
            Err(error) => {
                let mut io = io.lock().await;
                let frame = failure(&error, io.operation);
                let reusable = frame["rolledBack"] == true;
                io.send(frame).await?;
                if io.expired || !reusable || store.is_closed() {
                    return Ok(());
                }
            }
        }
    }
}
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        // stdout stays protocol-only; driver diagnostics and credentials never escape.
        let mut output = tokio::io::stdout();
        if let Ok(mut bytes) = serde_json::to_vec(&failure(&error, None)) {
            bytes.push(b'\n');
            let _ = output.write_all(&bytes).await;
            let _ = output.flush().await;
        }
        std::process::exit(1);
    }
    // Tokio stdin may have a blocking read outstanding after idle retirement.
    // Runtime shutdown waits for that worker; process exit closes the pipes now.
    std::process::exit(0);
}
