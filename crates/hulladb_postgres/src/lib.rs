//! Authoritative PostgreSQL commands and bounded queries. See docs/postgres-slice.md.
//! This is not a general storage adapter or a public SDK API.

mod model;
mod query;
mod store;

pub use model::{Change, Command, Error, Page, QueryRevision, Receipt, Session, Task};
pub use query::{QueryCache, QuerySnapshot, QueryStatus, QueryView, TaskQuery, TaskSummary};
pub use store::Store;
pub use uuid::Uuid;

pub const MAX_PAGE: u16 = 100;
const GENERATION: i32 = 2;
