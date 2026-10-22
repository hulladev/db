//! Self-hosted registered-function starter. Operator commands are separate from
//! serving; the server never reads the operator credential file. See deployment docs.
#[path = "selfhost/application.rs"]
mod application;
use axum::{Router, http::StatusCode, routing::get};
use hulladb::{Context, Error, ManagedSchema, ManagedStore, Roles, ScopeQuota, Uuid};
use hulladb_server::{
    AuthenticationError, HostOptions,
    managed::{
        AuthenticationFuture, Authenticator, CredentialHeaders, ManagedHost, Options,
        PrincipalQuotas,
    },
};
use serde::Deserialize;
use std::{collections::BTreeSet, path::PathBuf, sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    bearer: String,
    scope: Uuid,
    principal: Uuid,
    authorization_scope: Uuid,
    identity: String,
}
fn credentials(path: &std::path::Path) -> Result<Vec<Credential>> {
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(std::fs::File::open(path)?, 1_048_577),
        &mut bytes,
    )?;
    if bytes.len() > 1_048_576 {
        return Err("credential file exceeds budget".into());
    }
    let values: Vec<Credential> = serde_json::from_slice(&bytes)?;
    let mut tokens = BTreeSet::new();
    if values.is_empty()
        || values.len() > 4096
        || values.iter().any(|v| {
            v.bearer.len() < 32
                || v.bearer.len() > 256
                || v.bearer.bytes().any(|b| b.is_ascii_whitespace())
                || v.identity.is_empty()
                || v.identity.len() > 256
                || !tokens.insert(v.bearer.clone())
        })
    {
        return Err("invalid operator-supplied credential file".into());
    }
    Ok(values)
}
impl Credential {
    fn context(&self) -> Context<String> {
        Context {
            scope: self.scope,
            principal: self.principal,
            authorization_scope: self.authorization_scope,
            identity: self.identity.clone(),
        }
    }
}
struct FileAuth(PathBuf);
impl Authenticator<String> for FileAuth {
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

            let path = self.0.clone();
            let values = tokio::task::spawn_blocking(move || {
                credentials(&path).map_err(|_| AuthenticationError::Unavailable)
            })
            .await
            .map_err(|_| AuthenticationError::Unavailable)??;
            let mut context = None;
            for value in values {
                let mut difference = value.bearer.len() ^ bearer.len();
                for (i, byte) in value.bearer.bytes().enumerate() {
                    difference |=
                        usize::from(byte ^ bearer.as_bytes().get(i).copied().unwrap_or(0));
                }
                if difference == 0 {
                    context = Some(value.context());
                }
            }
            context.ok_or(AuthenticationError::InvalidCredentials)
        })
    }
}
fn env(name: &str) -> Result<String> {
    std::env::var(name).map_err(|_| format!("required configuration {name} is absent").into())
}
fn secret(name: &str) -> Result<String> {
    let path = env(name)?;
    let value = std::fs::read_to_string(path)?.trim().to_owned();
    if value.is_empty() {
        return Err("empty credential file".into());
    }
    Ok(value)
}
fn number(name: &str, fallback: usize) -> Result<usize> {
    match std::env::var(name) {
        Ok(value) => {
            let value = value.parse()?;
            if value == 0 {
                return Err(format!("{name} must be positive").into());
            }
            Ok(value)
        }
        Err(_) => Ok(fallback),
    }
}
fn schema() -> Result<ManagedSchema> {
    Ok(ManagedSchema::new(
        std::env::var("HULLADB_NAMESPACE").unwrap_or_else(|_| "hulladb_app".into()),
        hulladb::schema!(application::Entry)?,
    )?)
}
fn quota() -> Result<ScopeQuota> {
    Ok(ScopeQuota {
        max_rows: Some(i64::try_from(number("HULLADB_MAX_ROWS", 100_000)?)?),
        max_data_bytes: Some(i64::try_from(number(
            "HULLADB_MAX_DATA_BYTES",
            134_217_728,
        )?)?),
        max_receipt_bytes: Some(i64::try_from(number(
            "HULLADB_MAX_RECEIPT_BYTES",
            134_217_728,
        )?)?),
        max_outbox_jobs: Some(i64::try_from(number("HULLADB_MAX_OUTBOX_JOBS", 10_000)?)?),
        max_outbox_bytes: Some(i64::try_from(number(
            "HULLADB_MAX_OUTBOX_BYTES",
            16_777_216,
        )?)?),
        max_result_bytes: Some(i64::try_from(number("HULLADB_MAX_RESULT_BYTES", 32768)?)?),
    })
}
async fn initialize() -> Result<()> {
    let url = secret("HULLADB_ADMIN_DATABASE_URL_FILE")?;
    let mut store = ManagedStore::connect(&url, schema()?).await?;
    store.initialize().await?;
    let path = PathBuf::from(env("HULLADB_CREDENTIALS_FILE")?);
    let mut scopes = BTreeSet::new();
    for credential in credentials(&path)? {
        if !scopes.insert(credential.scope) {
            continue;
        }
        match store.managed_head(&credential.context()).await {
            Ok(_) => {}
            Err(Error::NotFound) => store.create_scope(credential.scope).await?,
            Err(error) => return Err(error.into()),
        }
        store.set_scope_quota(credential.scope, quota()?).await?;
    }
    let roles = Roles {
        worker: std::env::var("HULLADB_WORKER_ROLE")
            .unwrap_or_else(|_| "hulladb_worker_group".into()),
        owner: std::env::var("HULLADB_OWNER_ROLE").unwrap_or_else(|_| "hulladb_owner".into()),
        runtime: std::env::var("HULLADB_RUNTIME_ROLE")
            .unwrap_or_else(|_| "hulladb_runtime_group".into()),
        backup: std::env::var("HULLADB_BACKUP_ROLE")
            .unwrap_or_else(|_| "hulladb_backup_group".into()),
    };
    store.provision_roles(&roles).await?;
    if let Ok(login) = std::env::var("HULLADB_WORKER_LOGIN") {
        store.grant_role(&roles.worker, &login).await?;
    }
    store
        .grant_role(
            &roles.runtime,
            &std::env::var("HULLADB_RUNTIME_LOGIN").unwrap_or_else(|_| "hulladb_runtime".into()),
        )
        .await?;
    store
        .grant_role(
            &roles.backup,
            &std::env::var("HULLADB_BACKUP_LOGIN").unwrap_or_else(|_| "hulladb_backup".into()),
        )
        .await?;
    println!("Schema, scopes, quotas and role grants are ready");
    Ok(())
}
async fn operate(args: &[String]) -> Result<()> {
    let usage = "use status SCOPE_UUID or compact-receipts SCOPE_UUID EXPECTED_SOURCE_UUID MINIMUM_AGE_SECONDS LIMIT";
    if !matches!(args, [command, _] if command == "status")
        && !matches!(args, [command, _, _, _, _] if command == "compact-receipts")
    {
        return Err(usage.into());
    }
    let scope: Uuid = args[1].parse()?;
    let url = secret("HULLADB_ADMIN_DATABASE_URL_FILE")?;
    let mut store = ManagedStore::connect(&url, schema()?).await?;
    store.verify().await?;
    if args[0] == "status" {
        println!(
            "{}",
            serde_json::to_string(&store.scope_status(scope).await?)?
        );
    } else {
        let source: Uuid = args[2].parse()?;
        let age = Duration::from_secs(args[3].parse()?);
        let limit: u16 = args[4].parse()?;
        let compacted = store.compact_receipts(scope, source, age, limit).await?;
        println!("{}", serde_json::json!({"compacted": compacted}));
    }
    Ok(())
}
async fn serve() -> Result<()> {
    let url = secret("HULLADB_DATABASE_URL_FILE")?;
    let path = PathBuf::from(env("HULLADB_CREDENTIALS_FILE")?);
    credentials(&path)?;
    let budgets = HostOptions {
        poll_interval: Duration::from_millis(1000),
        credential_check_interval: Duration::from_millis(1000),
        operation_timeout: Duration::from_secs(10),
        database_connections: number("HULLADB_CONNECTIONS", 4)?,
        max_inflight_requests: number("HULLADB_MAX_REQUESTS", 64)?,
        max_subscriptions: number("HULLADB_MAX_SUBSCRIPTIONS", 64)?,
        max_body_bytes: 65536,
    };
    let origins = std::env::var("HULLADB_CORS_ORIGINS")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let host = Arc::new(
        ManagedHost::connect(
            &url,
            schema()?,
            application::registry()?,
            Arc::new(FileAuth(path)),
            Options {
                budgets,
                cors_origins: origins,
                principal_quotas: Some(PrincipalQuotas {
                    requests_per_minute: u32::try_from(number(
                        "HULLADB_REQUESTS_PER_MINUTE",
                        120,
                    )?)?,
                    max_inflight: number("HULLADB_PRINCIPAL_INFLIGHT", 8)?,
                    max_subscriptions: number("HULLADB_PRINCIPAL_SUBSCRIPTIONS", 8)?,
                    max_identities: number("HULLADB_MAX_IDENTITIES", 4096)?,
                }),
            },
        )
        .await?,
    );
    let ready = host.clone();
    let router = Router::new()
        .route("/health/live", get(|| async { StatusCode::OK }))
        .route(
            "/health/ready",
            get(move || {
                let host = ready.clone();
                async move {
                    if host.ready().await {
                        StatusCode::OK
                    } else {
                        StatusCode::SERVICE_UNAVAILABLE
                    }
                }
            }),
        )
        .merge(host.router());
    let address = std::env::var("HULLADB_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!("Serving registered functions on {address}");
    let report_seconds = number("HULLADB_STATS_INTERVAL_SECONDS", 30)?;
    if report_seconds > 86400 {
        return Err("stats interval must be 1..86400 seconds".into());
    }
    let report_interval = Duration::from_secs(report_seconds as u64);
    let mut reports = tokio::time::interval_at(
        tokio::time::Instant::now() + report_interval,
        report_interval,
    );
    reports.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let reporting = host.clone();
    let serving = async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                #[cfg(unix)]
                {
                    let mut terminate =
                        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                            .expect("termination signal");
                    tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
                }
                #[cfg(not(unix))]
                {
                    let _ = tokio::signal::ctrl_c().await;
                }
                host.stop();
            })
            .await
    };
    tokio::pin!(serving);
    loop {
        tokio::select! {
            result = &mut serving => { result?; break; }
            _ = reports.tick() => {
                println!("{}", serde_json::json!({"event":"hulladb.host_stats", "stats":reporting.stats()}));
            }
        }
    }
    Ok(())
}
async fn healthcheck() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let port = number("HULLADB_PORT", 8080)?;
        let mut connection =
            tokio::net::TcpStream::connect(("127.0.0.1", u16::try_from(port)?)).await?;
        connection
            .write_all(
                b"GET /health/ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await?;
        let mut response = [0u8; 128];
        let mut size = 0;
        while size < response.len() && !response[..size].contains(&b'\n') {
            let received = connection.read(&mut response[size..]).await?;
            if received == 0 {
                break;
            }
            size += received;
        }
        if !response[..size].starts_with(b"HTTP/1.1 200") {
            return Err("readiness probe failed".into());
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    })
    .await??;
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("init") => initialize().await,
        Some("serve") => serve().await,
        Some("healthcheck") => healthcheck().await,
        Some("status" | "compact-receipts") => operate(&args).await,
        _ => Err("use init, serve, healthcheck, status or compact-receipts".into()),
    }
}
