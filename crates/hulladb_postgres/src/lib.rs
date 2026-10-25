//! Schema-driven PostgreSQL execution, transactions and recovery.
pub mod managed;

/// Authenticated HTTP/SSE hosting, enabled by the `http` feature.
#[cfg(feature = "http")]
pub mod hosting;
