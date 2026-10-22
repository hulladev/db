use crate::{ValidationErrorKind, Value};

/// Initial portable scalar descriptors, independent of database driver types.
///
/// This is an implementation subset, not the complete planned validator system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarType {
    String,
    Float64,
    Json,
    Boolean,
    /// A signed integer in the range -2^63 through 2^63 - 1.
    Int64,
    /// A UUID value. Its generation algorithm is specified separately.
    Uuid,
    Date,
    Timestamp,
    LocalTimestamp,
}

/// A declared field. Missing insert/update values are a separate input concern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDefinition {
    pub constraints: crate::FieldConstraints,
    pub name: String,
    pub scalar_type: ScalarType,
    pub nullable: bool,
    /// Fractional second precision for timestamps (0..6); absent otherwise.
    pub precision: Option<u8>,
    pub json_shape: Option<crate::JsonShape>,
    /// A literal used for an omitted insert field. `None` requires presence;
    /// `Some(Value::Null)` is an explicit null default for a nullable field.
    /// Primary keys use key generation instead of field defaults.
    pub default: Option<Value>,
}

impl FieldDefinition {
    /// Declare a field that does not permit null.
    pub fn new(name: impl Into<String>, scalar_type: ScalarType) -> Self {
        Self {
            constraints: crate::FieldConstraints::default(),
            name: name.into(),
            scalar_type,
            nullable: false,
            precision: matches!(
                scalar_type,
                ScalarType::Timestamp | ScalarType::LocalTimestamp
            )
            .then_some(3),
            json_shape: None,
            default: None,
        }
    }

    /// Check a supplied value or a literal default against this field.
    pub fn validate_value(&self, value: &Value) -> Result<(), ValidationErrorKind> {
        match value.scalar_type() {
            None if !self.nullable => Err(ValidationErrorKind::NullNotAllowed),
            Some(actual) if actual != self.scalar_type => Err(ValidationErrorKind::TypeMismatch {
                expected: self.scalar_type,
                actual,
            }),
            _ => {
                let temporal_valid = match value {
                    Value::Date(text) => crate::temporal::date(text).is_some(),
                    Value::Timestamp(text) => {
                        crate::temporal::timestamp(text, self.precision.unwrap_or(3), true)
                            .is_some()
                    }
                    Value::LocalTimestamp(text) => {
                        crate::temporal::timestamp(text, self.precision.unwrap_or(3), false)
                            .is_some()
                    }
                    _ => true,
                };
                if !temporal_valid {
                    return Err(ValidationErrorKind::InvalidTemporal);
                }
                if let Value::Json(value) = value {
                    if value.is_null() {
                        return Err(ValidationErrorKind::InvalidJson {
                            path: "$".into(),
                            message: "top-level null must use Value::Null".into(),
                        });
                    }
                    self.json_shape
                        .as_ref()
                        .unwrap_or(&crate::JsonShape::Any)
                        .validate(value)?;
                }
                if !self.constraints.accepts(value) {
                    return Err(ValidationErrorKind::ConstraintViolation);
                }
                Ok(())
            }
        }
    }
}
