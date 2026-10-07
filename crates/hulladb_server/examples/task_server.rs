//! Loopback development host for an explicitly initialized task workspace.
use std::{sync::Arc, time::Duration};

use hulladb_protocol::Session;
use hulladb_server::{
    AuthenticationError, AuthenticationFuture, Authenticator, HostOptions, TaskHost,
};

struct DevelopmentCredential {
    bearer: String,
    session: Session,
}
impl Authenticator for DevelopmentCredential {
    fn authenticate<'a>(&'a self, bearer: &'a str) -> AuthenticationFuture<'a> {
        Box::pin(async move {
            if bearer == self.bearer {
                Ok(self.session)
            } else {
                Err(AuthenticationError::InvalidCredentials)
            }
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = std::env::var("HULLADB_DATABASE_URL")?.parse()?;
    let bearer = std::env::var("HULLADB_DEV_BEARER")?;
    if bearer.is_empty() || bearer.bytes().any(|b| b.is_ascii_whitespace()) {
        return Err("HULLADB_DEV_BEARER must be a nonempty token without whitespace".into());
    }
    let session = Session {
        workspace: std::env::var("HULLADB_DEV_WORKSPACE")?.parse()?,
        principal: std::env::var("HULLADB_DEV_PRINCIPAL")?.parse()?,
    };
    let host = Arc::new(
        TaskHost::connect(
            config,
            Arc::new(DevelopmentCredential { bearer, session }),
            HostOptions {
                poll_interval: Duration::from_millis(250),
                credential_check_interval: Duration::from_secs(5),
                operation_timeout: Duration::from_secs(10),
                database_connections: 4,
                max_inflight_requests: 64,
                max_subscriptions: 128,
                max_body_bytes: 16 * 1024,
            },
        )
        .await?,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8787").await?;
    println!("Task development host listening on http://127.0.0.1:8787");
    let stopping = host.clone();
    axum::serve(listener, host.router())
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            stopping.stop();
        })
        .await?;
    Arc::try_unwrap(host)
        .map_err(|_| "host owner still retained")?
        .shutdown()
        .await;
    Ok(())
}
