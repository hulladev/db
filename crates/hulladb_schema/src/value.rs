use std::collections::BTreeMap;

use crate::ScalarType;

/// A decoded logical value, independent of SDK, wire, and database encodings.
///
/// There is no missing-value variant: omission is absence from [`RecordValues`].
/// Decoders must preserve integer precision and parse UUIDs before constructing
/// these values. UUID version restrictions are not part of the current contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Null,
    String(String),
    Date(String),
    Timestamp(String),
    LocalTimestamp(String),
    Boolean(bool),
    Int64(i64),
    Float64(serde_json::Number),
    Json(serde_json::Value),
    /// The 16 bytes of a UUID, rather than an unvalidated text string.
    Uuid([u8; 16]),
}

impl Value {
    pub fn json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text).map(Self::Json)
    }
    pub fn float64(value: f64) -> Option<Self> {
        serde_json::Number::from_f64(if value == 0.0 { 0.0 } else { value }).map(Self::Float64)
    }
    pub(crate) fn scalar_type(&self) -> Option<ScalarType> {
        match self {
            Self::Null => None,
            Self::String(_) => Some(ScalarType::String),
            Self::Date(_) => Some(ScalarType::Date),
            Self::Timestamp(_) => Some(ScalarType::Timestamp),
            Self::LocalTimestamp(_) => Some(ScalarType::LocalTimestamp),
            Self::Boolean(_) => Some(ScalarType::Boolean),
            Self::Int64(_) => Some(ScalarType::Int64),
            Self::Float64(_) => Some(ScalarType::Float64),
            Self::Json(_) => Some(ScalarType::Json),
            Self::Uuid(_) => Some(ScalarType::Uuid),
        }
    }
}

/// Named values for a complete row, insert input, or partial update.
///
/// In an update, absence means unchanged, null means clear a nullable field, and
/// a scalar value means replace. Complete rows do not permit absent fields.
///
/// A decoder must reject duplicate field names before constructing this map.
/// This representation does not prescribe a network serialization format.
pub type RecordValues = BTreeMap<String, Value>;
