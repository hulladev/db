//! Diagnostic PostgreSQL logical capture and bounded durable raw history.
//!
//! This crate is not part of managed synchronization. It exercises pgoutput v1
//! capture/recovery; the optional raw replica is an inspection probe. Production
//! task delivery lives in `hulladb_sync` and has no dependency on this crate.

mod bootstrap;
mod capture;
mod log;
mod model;
mod pgoutput;
#[cfg(feature = "diagnostic-replica")]
mod replica;
#[cfg(feature = "diagnostic-replica")]
mod snapshot;

pub use capture::{Capture, Progress};
pub use log::{
    EventLog, ReadBudget, Retention, StorageOptions, StorageStats, StoredTransaction,
    TransactionHeader,
};
pub use model::{Batch, Change, Column, Cursor, Error, Event, Lsn, Relation, Source, Value};
#[cfg(feature = "diagnostic-replica")]
pub use replica::{Replica, ReplicaView};
#[cfg(feature = "diagnostic-replica")]
pub use snapshot::{MAX_SNAPSHOT_BYTES, MAX_SNAPSHOT_ROWS, Snapshot, SnapshotReader};
