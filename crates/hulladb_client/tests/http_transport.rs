#![cfg(feature = "http")]
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use hulladb_client::http::{
    ClientOptions, Endpoint, Error,
    generic::{HttpTransport, SseFrame},
};
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{net::TcpListener, task::JoinHandle, time::timeout};

struct Server {
    address: SocketAddr,
    task: JoinHandle<()>,
}
impl Server {
    async fn start(router: Router, tls: Option<Arc<tokio_rustls::rustls::ServerConfig>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            if let Some(config) = tls {
                let acceptor = tokio_rustls::TlsAcceptor::from(config);
                let mut connections = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        accepted = listener.accept() => {
                            let (socket, _) = accepted.unwrap();
                            let acceptor = acceptor.clone();
                            let router = router.clone();
                            connections.spawn(async move {
                                if let Ok(socket) = acceptor.accept(socket).await {
                                    let service = hyper_util::service::TowerToHyperService::new(router);
                                    let _ = hyper::server::conn::http1::Builder::new()
                                        .serve_connection(hyper_util::rt::TokioIo::new(socket), service).await;
                                }
                            });
                        },
                        _ = connections.join_next(), if !connections.is_empty() => {},
                    }
                }
            } else {
                axum::serve(listener, router).await.unwrap();
            }
        });
        Self { address, task }
    }
    fn endpoint(&self) -> Endpoint {
        format!("https://localhost:{}", self.address.port())
            .parse()
            .unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn json_router(visits: Arc<AtomicUsize>) -> Router {
    Router::new().fallback(move || {
        visits.fetch_add(1, Ordering::Relaxed);
        async { axum::Json(serde_json::json!({"accepted": true})) }
    })
}
struct Certificate {
    config: Arc<tokio_rustls::rustls::ServerConfig>,
    root: Vec<u8>,
}
impl Certificate {
    fn new(names: Vec<String>, expired: bool) -> Self {
        use rcgen::{
            BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair, KeyUsagePurpose,
        };
        use tokio_rustls::rustls::{self, pki_types::PrivatePkcs8KeyDer};
        let mut ca = CertificateParams::new(Vec::new()).unwrap();
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let issuer = CertifiedIssuer::self_signed(ca, KeyPair::generate().unwrap()).unwrap();
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(names).unwrap();
        if expired {
            params.not_before = rcgen::date_time_ymd(1999, 1, 1);
            params.not_after = rcgen::date_time_ymd(2000, 1, 1);
        }
        let cert = params.signed_by(&key, &issuer).unwrap();
        let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        )
        .unwrap();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Self {
            config: Arc::new(config),
            root: issuer.der().to_vec(),
        }
    }
}

#[tokio::test]
async fn https_verifies_private_ca_and_dns_origin_for_json_and_streams() {
    let cert = Certificate::new(vec!["localhost".into()], false);
    let (observed, mut requests) = tokio::sync::mpsc::channel(4);
    let router = Router::new().fallback(move |headers: HeaderMap| {
        let observed = observed.clone();
        async move {
            observed.send(headers.clone()).await.unwrap();
            if headers["accept"] == "text/event-stream" {
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(Body::from("event: result\ndata: {\"value\":42}\n\n"))
                    .unwrap()
            } else {
                Response::builder()
                    .header("content-type", "application/json")
                    .body(Body::from("{\"value\":42}"))
                    .unwrap()
            }
        }
    });
    let server = Server::start(router, Some(cert.config)).await;
    let identity = [
        uuid::Uuid::now_v7(),
        uuid::Uuid::now_v7(),
        uuid::Uuid::now_v7(),
    ];
    let client = HttpTransport::new(
        server.endpoint().with_trusted_roots([cert.root]).unwrap(),
        "secure",
        ClientOptions::default(),
    )
    .unwrap()
    .with_identity(identity[0], identity[1], identity[2]);
    assert_eq!(
        client
            .json("POST", "/functions/call", b"{}".to_vec())
            .await
            .unwrap(),
        b"{\"value\":42}"
    );
    let mut stream = client
        .stream("/functions/subscribe", b"{}".to_vec())
        .await
        .unwrap();
    assert!(
        matches!(stream.next().await.unwrap(), SseFrame::Event { name, data, .. } if name == "result" && data == "{\"value\":42}")
    );
    for _ in 0..2 {
        let headers = requests.recv().await.unwrap();
        assert_eq!(
            headers["host"],
            format!("localhost:{}", server.address.port())
        );
        assert_eq!(headers["authorization"], "Bearer secure");
        for (header, id) in [
            "x-hulladb-scope",
            "x-hulladb-principal",
            "x-hulladb-authorization-scope",
        ]
        .into_iter()
        .zip(identity)
        {
            assert_eq!(headers[header], id.to_string());
        }
    }
}

#[tokio::test]
async fn tls_rejects_unknown_ca_wrong_name_and_expired_certificates_before_http_submission() {
    for (name, expired, trust) in [
        ("localhost", false, false),
        ("wrong.example", false, true),
        ("localhost", true, true),
    ] {
        let cert = Certificate::new(vec![name.into()], expired);
        let visits = Arc::new(AtomicUsize::new(0));
        let server = Server::start(json_router(visits.clone()), Some(cert.config)).await;
        let mut endpoint = server.endpoint();
        if trust {
            endpoint = endpoint.with_trusted_roots([cert.root]).unwrap();
        }
        let client = HttpTransport::new(endpoint, "secret", ClientOptions::default()).unwrap();
        assert!(matches!(
            client.json("POST", "/functions/call", b"{}".to_vec()).await,
            Err(Error::Tls)
        ));
        assert!(matches!(
            client.stream("/functions/subscribe", b"{}".to_vec()).await,
            Err(Error::Tls)
        ));
        assert_eq!(visits.load(Ordering::Relaxed), 0);
    }
}

#[tokio::test]
async fn tls_handshake_deadline_and_cancellation_close_the_socket_without_sending_credentials() {
    use tokio::io::AsyncReadExt;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    )
    .parse()
    .unwrap();
    let client = HttpTransport::new(
        endpoint,
        "secret",
        ClientOptions {
            request_timeout: Duration::from_millis(60),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(
        client.json("POST", "/functions/call", b"{}".to_vec()).await,
        Err(Error::Timeout)
    ));
    let (mut socket, _) = listener.accept().await.unwrap();
    let mut bytes = Vec::new();
    timeout(Duration::from_secs(1), socket.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    assert!(
        !bytes
            .windows(b"Bearer secret".len())
            .any(|part| part == b"Bearer secret")
    );
    let pending =
        tokio::spawn(async move { client.stream("/functions/subscribe", b"{}".to_vec()).await });
    let (mut socket, _) = listener.accept().await.unwrap();
    pending.abort();
    let _ = pending.await;
    timeout(Duration::from_secs(1), socket.read_to_end(&mut Vec::new()))
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn endpoint_configuration_rejects_plaintext_remote_origins_and_ambiguous_urls() {
    for origin in [
        "http://example.com",
        "http://192.0.2.1:8787",
        "http://localhost:8787",
        "https://user:pass@localhost",
        "https://localhost/tasks",
        "https://localhost?token=secret",
        "https://localhost#secret",
        "ftp://localhost",
        "localhost:8787",
        "https://localhost:0",
        "https://localhost:99999",
        "https://localhost:",
        "https://localhost:+443",
        "https://[::1]:99999",
    ] {
        assert!(
            matches!(origin.parse::<Endpoint>(), Err(Error::InvalidEndpoint)),
            "{origin}"
        );
    }
    assert!(matches!(
        Endpoint::local("192.0.2.1:8787".parse().unwrap()),
        Err(Error::InvalidEndpoint)
    ));
    for origin in [
        "https://example.com",
        "https://127.0.0.1:8787/",
        "https://[::1]:8787",
        "http://127.0.0.1:8787",
        "http://[::1]:8787/",
    ] {
        assert!(origin.parse::<Endpoint>().is_ok(), "{origin}");
    }
    let endpoint: Endpoint = "https://localhost".parse().unwrap();
    assert!(matches!(
        endpoint.clone().with_trusted_roots(Vec::new()),
        Err(Error::InvalidEndpoint)
    ));
    assert!(matches!(
        endpoint.with_trusted_roots([vec![1, 2, 3]]),
        Err(Error::InvalidEndpoint)
    ));
}

#[tokio::test]
async fn redirects_are_not_followed_or_allowed_to_forward_bearer_credentials() {
    let visits = Arc::new(AtomicUsize::new(0));
    let target = Server::start(json_router(visits.clone()), None).await;
    let address = target.address;
    let router = Router::new().fallback(move || async move {
        Response::builder()
            .status(StatusCode::TEMPORARY_REDIRECT)
            .header("location", format!("http://{address}/functions/call"))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap()
    });
    let server = Server::start(router, None).await;
    let client = HttpTransport::new(
        Endpoint::local(server.address).unwrap(),
        "secret",
        ClientOptions::default(),
    )
    .unwrap();
    assert!(matches!(
        client.json("POST", "/functions/call", b"{}".to_vec()).await,
        Err(Error::Protocol)
    ));
    assert!(matches!(
        client.stream("/functions/subscribe", b"{}".to_vec()).await,
        Err(Error::Protocol)
    ));
    assert_eq!(visits.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn https_literal_ip_requires_a_matching_ip_subject_alternative_name() {
    for name in ["127.0.0.1", "localhost"] {
        let cert = Certificate::new(vec![name.into()], false);
        let visits = Arc::new(AtomicUsize::new(0));
        let server = Server::start(json_router(visits.clone()), Some(cert.config)).await;
        let endpoint: Endpoint = format!("https://{}", server.address).parse().unwrap();
        let client = HttpTransport::new(
            endpoint.with_trusted_roots([cert.root]).unwrap(),
            "secret",
            ClientOptions::default(),
        )
        .unwrap();
        let result = client.json("POST", "/functions/call", b"{}".to_vec()).await;
        if name == "127.0.0.1" {
            assert!(result.is_ok());
            assert_eq!(visits.load(Ordering::Relaxed), 1);
        } else {
            assert!(matches!(result, Err(Error::Tls)));
            assert_eq!(visits.load(Ordering::Relaxed), 0);
        }
    }
}
