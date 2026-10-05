//! Internal PostgreSQL CDC capture implementation. This is not an authorized client feed.
//!
//! Captures pgoutput v1 transactions through a persistent replication connection and
//! persists them in a separate SQLite log before sending durable feedback.
//! A bounded snapshot/replica implementation uses an overlapping MVCC visibility fence.
//! It does not implement a public network protocol or client authorization.

mod capture;
mod log;
mod model;
mod pgoutput;
mod replica;
mod snapshot;

pub use capture::{Capture, Progress};
pub use log::{EventLog, Retention, StorageStats};
pub use model::{Batch, Change, Column, Cursor, Error, Event, Lsn, Relation, Source, Value};
pub use replica::{Replica, ReplicaView};
pub use snapshot::{MAX_SNAPSHOT_BYTES, MAX_SNAPSHOT_ROWS, Snapshot, SnapshotReader};

/// Bounds both accumulated input and the persisted JSON for one transaction.
pub const MAX_TRANSACTION_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TRANSACTION_CHANGES: usize = 10_000;
