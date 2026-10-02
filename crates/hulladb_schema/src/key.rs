use crate::ScalarType;

/// Schema-wide fallback for UUID primary keys without a table override.
///
/// This chooses generation only; it does not restrict supplied UUID versions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UuidGeneration {
    V4,
    #[default]
    V7,
}

impl UuidGeneration {
    pub(crate) fn key_generation(self) -> KeyGeneration {
        match self {
            Self::V4 => KeyGeneration::UuidV4,
            Self::V7 => KeyGeneration::UuidV7,
        }
    }
}

/// How the server produces an omitted primary key.
///
/// These are descriptors; normalization never executes a generator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyGeneration {
    /// No generator: inserts must supply a key.
    None,
    UuidV7,
    UuidV4,
    /// Reference to a server handler, with its declared output type.
    /// Handler registration and runtime output validation are later stages.
    Custom {
        handler: String,
        output_type: ScalarType,
    },
}

impl KeyGeneration {
    pub(crate) fn output_type(&self) -> Option<ScalarType> {
        match self {
            Self::None => None,
            Self::UuidV7 | Self::UuidV4 => Some(ScalarType::Uuid),
            Self::Custom { output_type, .. } => Some(*output_type),
        }
    }
}

/// Whether an insert may include an explicit primary-key value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InsertKeyPolicy {
    /// Accept a supplied key; use the generator when omitted. Without a
    /// generator, omission is an insert error.
    #[default]
    AllowExplicit,
    /// Reject supplied keys. Requires a configured generator.
    GeneratedOnly,
}
