//! Authorized query delivery from durable managed-transaction revisions.
//!
//! PostgreSQL remains authoritative; clients persist results in SQLite. The host
//! must poll refresh regularly, including after reconnect. No notification or
//! process-local signal is required for correctness. This is the fixed internal
//! task slice, not a general query engine, network service or SDK.
mod task_feed;
pub use hulladb_protocol::{DeliveryCursor, TaskMessage, TaskResult};
pub use task_feed::{TaskFeed, TaskFeedError, TaskSubscription};
