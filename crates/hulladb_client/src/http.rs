//! Native HTTP/1 transport with verified HTTPS and loopback-only plaintext.
use http_body_util::{BodyExt, Full};
use hulladb_protocol::{ApiError, ApiErrorCode};
use hyper::{
    body::{Bytes, Incoming},
    client::conn::http1,
    http::{HeaderValue, Request, StatusCode, header},
};
use hyper_util::rt::TokioIo;
use std::{fmt, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    task::JoinHandle,
    time::timeout,
};
use uuid::Uuid;
mod endpoint;
pub mod generic;
pub use endpoint::Endpoint;

#[derive(Debug, Clone, Copy)]
pub struct ClientOptions {
    pub request_timeout: Duration,
    /// Must exceed the host heartbeat interval, including expected network delay.
    pub stream_idle_timeout: Duration,
    pub reconnect_delay: Duration,
    pub max_response_bytes: usize,
    pub max_event_bytes: usize,
    pub max_subscriptions: usize,
}

#[derive(Debug)]
pub enum Error {
    InvalidOptions,
    InvalidEndpoint,
    Tls,
    Network,
    Timeout,
    Protocol,
    Budget,
    Api(ApiErrorCode),
    Application(hulladb_protocol::ApplicationError),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidEndpoint => {
                "endpoint must be an HTTPS origin or literal loopback HTTP origin"
            }
            Self::Tls => "endpoint TLS verification or negotiation failed",
            Self::InvalidOptions => "invalid native client options or bearer credential",
            Self::Network => "HTTP transport unavailable",
            Self::Timeout => "HTTP client deadline exceeded",
            Self::Protocol => "invalid HTTP transport response",
            Self::Budget => "HTTP response byte budget exceeded",
            Self::Api(_) | Self::Application(_) => "function host rejected the request",
        })
    }
}
impl std::error::Error for Error {}
fn credential(bearer: &str) -> Result<HeaderValue, Error> {
    if bearer.is_empty() || bearer.bytes().any(|b| b.is_ascii_whitespace()) {
        return Err(Error::InvalidOptions);
    }
    let mut value =
        HeaderValue::from_str(&format!("Bearer {bearer}")).map_err(|_| Error::InvalidOptions)?;
    value.set_sensitive(true);
    Ok(value)
}
struct Http {
    sender: http1::SendRequest<Full<Bytes>>,
    connection: JoinHandle<()>,
}
impl Drop for Http {
    fn drop(&mut self) {
        self.connection.abort();
    }
}
impl Http {
    async fn connect(
        socket: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
    ) -> Result<Self, Error> {
        let (sender, connection) = http1::handshake(TokioIo::new(socket))
            .await
            .map_err(|_| Error::Network)?;
        Ok(Self {
            sender,
            connection: tokio::spawn(async move {
                let _ = connection.await;
            }),
        })
    }
}

async fn request(
    endpoint: &Endpoint,
    bearer: &HeaderValue,
    method: &str,
    path: &str,
    bytes: Vec<u8>,
    stream: bool,
    identity: Option<[Uuid; 3]>,
) -> Result<(Http, hyper::Response<Incoming>), Error> {
    let socket = TcpStream::connect((endpoint.host.as_str(), endpoint.port))
        .await
        .map_err(|_| Error::Network)?;
    socket.set_nodelay(true).map_err(|_| Error::Network)?;
    let mut http = if let Some((connector, server_name)) = &endpoint.tls {
        let socket = connector
            .connect(server_name.clone(), socket)
            .await
            .map_err(|error| {
                if error
                    .get_ref()
                    .is_some_and(|cause| cause.is::<tokio_rustls::rustls::Error>())
                {
                    Error::Tls
                } else {
                    Error::Network
                }
            })?;
        Http::connect(socket).await?
    } else {
        Http::connect(socket).await?
    };
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, &endpoint.authority)
        .header(header::AUTHORIZATION, bearer.clone())
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            header::ACCEPT,
            if stream {
                "text/event-stream"
            } else {
                "application/json"
            },
        );
    if let Some([scope, principal, authorization_scope]) = identity {
        request = request
            .header("x-hulladb-scope", scope.to_string())
            .header("x-hulladb-principal", principal.to_string())
            .header(
                "x-hulladb-authorization-scope",
                authorization_scope.to_string(),
            );
    }
    let request = request
        .body(Full::new(Bytes::from(bytes)))
        .map_err(|_| Error::Protocol)?;
    let response = http
        .sender
        .send_request(request)
        .await
        .map_err(|_| Error::Network)?;
    Ok((http, response))
}

async fn body_bytes(mut body: Incoming, limit: usize) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        if let Ok(chunk) = frame.map_err(|_| Error::Network)?.into_data() {
            if chunk.len() > limit.saturating_sub(bytes.len()) {
                return Err(Error::Budget);
            }
            bytes.extend_from_slice(&chunk);
        }
    }
    Ok(bytes)
}
