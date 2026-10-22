use std::sync::Arc;
use tokio_postgres_rustls::MakeRustlsConnect;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

pub(super) fn verified_tls() -> MakeRustlsConnect {
    let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("TLS protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    // The maintained adapter supplies tls-server-end-point channel binding for
    // SCRAM-SHA-256-PLUS, including URLs that require channel_binding=require.
    MakeRustlsConnect::new(config)
}
