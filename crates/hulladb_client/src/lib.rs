//! Compatibility entry point for the SDK HTTP/SSE transport.
//! New code can use `hulladb::http` with the `http` or `remote` feature.
#[cfg(feature = "http")]
pub use hulladb::http;
