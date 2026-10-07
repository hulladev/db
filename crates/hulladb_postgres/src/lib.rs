//! Authoritative PostgreSQL commands and bounded queries. See docs/postgres-slice.md.
//! This is not a general storage adapter or a public SDK API.

mod model;
mod query;
mod store;

pub use model::{Change, Command, Error, Page, QueryRevision, Receipt, Task};
pub use query::{ManagedHead, QueryCache, QuerySnapshot, QueryStatus, QueryView};
pub use store::Store;
pub use uuid::Uuid;

pub use hulladb_protocol::{MAX_PAGE, Session, TaskQuery, TaskSummary};
