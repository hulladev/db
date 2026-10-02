use crate::{ValidationErrorKind, Value};

/// Initial portable scalar descriptors, independent of database driver types.
///
/// This is an implementation subset, not the complete planned validator system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarType {
    String,
    Boolean,
    /// A signed integer in the range -2^63 through 2^63 - 1.
    Int64,
    /// A UUID value. Its generation algorithm is specified separately.
    Uuid,
}

/// A declared field. Missing insert/update values are a separate input concern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDefinition {
    pub name: String,
    pub scalar_type: ScalarType,
    pub nullable: bool,
    /// A literal used for an omitted insert field. `None` requires presence;
    /// `Some(Value::Null)` is an explicit null default for a nullable field.
    /// Primary keys use key generation instead of field defaults.
    pub default: Option<Value>,
}

impl FieldDefinition {
    /// Declare a field that does not permit null.
    pub fn new(name: impl Into<String>, scalar_type: ScalarType) -> Self {
        Self {
            name: name.into(),
            scalar_type,
            nullable: false,
            default: None,
        }
    }

    /// Check a supplied value or a literal default against this field.
    pub(crate) fn validate_value(&self, value: &Value) -> Result<(), ValidationErrorKind> {
        match value.scalar_type() {
            None if !self.nullable => Err(ValidationErrorKind::NullNotAllowed),
            Some(actual) if actual != self.scalar_type => Err(ValidationErrorKind::TypeMismatch {
                expected: self.scalar_type,
                actual,
            }),
            _ => Ok(()),
        }
    }
}
