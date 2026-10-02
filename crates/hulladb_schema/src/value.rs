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
    Boolean(bool),
    Int64(i64),
    /// The 16 bytes of a UUID, rather than an unvalidated text string.
    Uuid([u8; 16]),
}

impl Value {
    pub(crate) fn scalar_type(&self) -> Option<ScalarType> {
        match self {
            Self::Null => None,
            Self::String(_) => Some(ScalarType::String),
            Self::Boolean(_) => Some(ScalarType::Boolean),
            Self::Int64(_) => Some(ScalarType::Int64),
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
