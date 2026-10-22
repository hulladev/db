//! Backend-independent named-function results and native client failures.
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::BTreeSet;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub scope: Uuid,
    pub sequence: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub request_id: Uuid,
    pub source: Uuid,
    pub revision: Revision,
    pub result: Json,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageMetadata {
    pub range: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub source: Uuid,
    pub revision: Revision,
    pub principal: Uuid,
    pub authorization_scope: Uuid,
    pub result: Json,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<PageMetadata>,
    /// Server-internal tracked dependencies. Remote results do not establish local row coverage.
    pub tables: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientFailure {
    Network,
    Timeout,
    Tls,
    Protocol,
    Budget,
    Storage,
    Closed,
    Capacity,
    Unauthorized,
}
impl std::fmt::Display for ClientFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "native function client failure: {self:?}")
    }
}
impl std::error::Error for ClientFailure {}
