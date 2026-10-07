use std::{future::Future, pin::Pin};

use hulladb_protocol::Session;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticationError {
    InvalidCredentials,
    Unavailable,
}

pub type AuthenticationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Session, AuthenticationError>> + Send + 'a>>;

/// Trusted application integration. Implement signature/issuer/audience/expiry
/// checks here, or consult an opaque credential store. Request fields are not an
/// authentication source. Streams call this periodically and before data delivery.
pub trait Authenticator: Send + Sync + 'static {
    fn authenticate<'a>(&'a self, bearer: &'a str) -> AuthenticationFuture<'a>;
}
