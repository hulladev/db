use super::Error;
use hyper::http::Uri;
use std::{
    net::{IpAddr, SocketAddr},
    str::FromStr,
    sync::Arc,
};
use tokio_rustls::{
    TlsConnector,
    rustls::{
        self, ClientConfig, RootCertStore,
        pki_types::{CertificateDer, ServerName},
    },
};

/// A fixed HTTP origin. HTTPS verifies the origin name and certificate chain;
/// plaintext development endpoints must be literal loopback IP addresses.
/// Paths, userinfo, queries and fragments are not endpoint configuration.
#[derive(Clone)]
pub struct Endpoint {
    pub(super) host: String,
    pub(super) authority: String,
    pub(super) port: u16,
    pub(super) tls: Option<(TlsConnector, ServerName<'static>)>,
}
impl Endpoint {
    /// Stable credential-free origin for local result/journal partitioning.
    pub fn origin(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host.to_lowercase())
        } else {
            self.host.to_lowercase()
        };
        format!(
            "{}://{host}:{}",
            if self.tls.is_some() { "https" } else { "http" },
            self.port
        )
    }
    pub fn local(address: SocketAddr) -> Result<Self, Error> {
        Self::from_str(&format!("http://{address}"))
    }
    /// Replace the public trust roots with explicitly supplied DER certificates.
    /// Certificate chain, expiry and origin-name verification remain mandatory.
    /// This is useful for a private CA; it does not disable verification.
    pub fn with_trusted_roots(
        mut self,
        certificates: impl IntoIterator<Item = Vec<u8>>,
    ) -> Result<Self, Error> {
        let (_, server_name) = self.tls.take().ok_or(Error::InvalidEndpoint)?;
        let mut roots = RootCertStore::empty();
        for certificate in certificates {
            roots
                .add(CertificateDer::from(certificate))
                .map_err(|_| Error::InvalidEndpoint)?;
        }
        if roots.is_empty() {
            return Err(Error::InvalidEndpoint);
        }
        self.tls = Some((connector(roots)?, server_name));
        Ok(self)
    }
}
impl FromStr for Endpoint {
    type Err = Error;
    fn from_str(origin: &str) -> Result<Self, Error> {
        if origin.contains('#') {
            return Err(Error::InvalidEndpoint);
        }
        let uri: Uri = origin.parse().map_err(|_| Error::InvalidEndpoint)?;
        let authority = uri.authority().ok_or(Error::InvalidEndpoint)?;
        if authority.as_str().contains('@')
            || uri.query().is_some()
            || !matches!(uri.path(), "" | "/")
        {
            return Err(Error::InvalidEndpoint);
        }
        let raw_host = authority.host();
        let host = raw_host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        if host.is_empty() {
            return Err(Error::InvalidEndpoint);
        }
        // Authority::port() treats an invalid/overflowing port as absent. Parse
        // the explicit suffix so it cannot silently select a different origin.
        let suffix = &authority.as_str()[raw_host.len()..];
        let port = if suffix.is_empty() {
            None
        } else {
            let digits = suffix.strip_prefix(':').ok_or(Error::InvalidEndpoint)?;
            if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(Error::InvalidEndpoint);
            }
            Some(digits.parse::<u16>().map_err(|_| Error::InvalidEndpoint)?)
        };
        let (port, tls) = match uri.scheme_str() {
            Some("https") => {
                let server_name =
                    ServerName::try_from(host.clone()).map_err(|_| Error::InvalidEndpoint)?;
                let roots =
                    RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
                (port.unwrap_or(443), Some((connector(roots)?, server_name)))
            }
            Some("http") if host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()) => {
                (port.unwrap_or(80), None)
            }
            _ => return Err(Error::InvalidEndpoint),
        };
        if port == 0 {
            return Err(Error::InvalidEndpoint);
        }
        Ok(Self {
            host,
            port,
            authority: authority.as_str().to_owned(),
            tls,
        })
    }
}
fn connector(roots: RootCertStore) -> Result<TlsConnector, Error> {
    // Select a provider on this configuration, rather than altering the embedding
    // process's global provider. Early data stays disabled for mutations.
    let mut config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|_| Error::InvalidEndpoint)?
            .with_root_certificates(roots)
            .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsConnector::from(Arc::new(config)))
}
