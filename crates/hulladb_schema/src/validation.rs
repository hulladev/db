use std::fmt;

use crate::{InsertKeyPolicy, KeyGeneration, RecordValues, ScalarType, TableContract, Value};

/// What remains to be done with an insert's primary key after input validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertKeyAction {
    /// The supplied key passed validation.
    Provided,
    /// The configured generator must run before validating the complete row.
    Generate,
}

/// Validated insert values with literal defaults filled in.
///
/// An omitted generated key is still pending. The runtime must add it and call
/// [`TableContract::validate_row`] before storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedInsert {
    values: RecordValues,
    key_action: InsertKeyAction,
}

impl PreparedInsert {
    pub fn values(&self) -> &RecordValues {
        &self.values
    }

    pub fn key_action(&self) -> InsertKeyAction {
        self.key_action
    }

    pub fn into_values(self) -> RecordValues {
        self.values
    }
}

/// A value diagnostic, distinct from errors in the schema declaration itself.
///
/// Row and insert validation check insert-key policy first (when applicable),
/// then declared values in schema order, then unknown names in lexicographic
/// order. Update validation rejects a supplied primary key first, then checks
/// supplied fields in lexicographic order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub table: String,
    pub field: String,
    pub kind: ValidationErrorKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationErrorKind {
    UnknownField,
    MissingField,
    NullNotAllowed,
    TypeMismatch {
        expected: ScalarType,
        actual: ScalarType,
    },
    ExplicitKeyForbidden,
    PrimaryKeyImmutable,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "table {:?}, field {:?}: ", self.table, self.field)?;
        match &self.kind {
            ValidationErrorKind::UnknownField => write!(f, "field is not declared"),
            ValidationErrorKind::MissingField => write!(
                f,
                "a value is required; nullability does not permit omission"
            ),
            ValidationErrorKind::NullNotAllowed => write!(f, "null is not allowed"),
            ValidationErrorKind::TypeMismatch { expected, actual } => {
                write!(f, "expected {expected:?}, received {actual:?}")
            }
            ValidationErrorKind::ExplicitKeyForbidden => write!(
                f,
                "an explicit primary key is forbidden; omit it to use generation"
            ),
            ValidationErrorKind::PrimaryKeyImmutable => {
                write!(f, "the primary key cannot be supplied in an update")
            }
        }
    }
}

impl std::error::Error for ValidationError {}

impl TableContract {
    /// Validate a complete row, including its primary key and nullable fields.
    ///
    /// Generated keys are required here too. This checks shape and scalar values;
    /// storage uniqueness and relationship existence remain separate checks.
    pub fn validate_row(&self, values: &RecordValues) -> Result<(), ValidationError> {
        self.validate_declared_values(values, None, false)?;
        Ok(())
    }

    /// Validate insert input and resolve whether its key must be generated.
    ///
    /// Fields require an explicit value unless a literal default is configured.
    /// This method neither changes the input nor executes generators. Use
    /// [`Self::prepare_insert`] to fill defaults before completing the row.
    pub fn validate_insert(
        &self,
        values: &RecordValues,
    ) -> Result<InsertKeyAction, ValidationError> {
        let action = self.resolve_insert_key_action(values)?;
        self.validate_insert_values(values, action)?;
        Ok(action)
    }

    /// Consume insert input, validate it, and fill omitted literal defaults.
    ///
    /// Supplied values always win, including an explicit null on a nullable
    /// field. Invalid supplied values are errors, never replaced by defaults.
    /// The schema is traversed once; only missing defaults are collected and
    /// cloned into the owned map after the entire input passes validation.
    pub fn prepare_insert(
        &self,
        mut values: RecordValues,
    ) -> Result<PreparedInsert, ValidationError> {
        let key_action = self.resolve_insert_key_action(&values)?;
        let defaults = self.validate_insert_values(&values, key_action)?;
        for (name, value) in defaults {
            values.insert(name.to_owned(), value.clone());
        }
        Ok(PreparedInsert { values, key_action })
    }

    /// Validate a partial update: omitted fields remain unchanged, explicit null
    /// clears a nullable field, and a scalar value replaces the existing value.
    ///
    /// The selected primary key must be omitted, even when its supplied value
    /// would be unchanged. An empty update is valid. Insert defaults and key
    /// generators do not apply. Only supplied fields are visited.
    pub fn validate_update(&self, changes: &RecordValues) -> Result<(), ValidationError> {
        let key = &self.primary_key().name;
        if changes.contains_key(key) {
            return Err(self.value_error(key, ValidationErrorKind::PrimaryKeyImmutable));
        }
        for (name, value) in changes {
            let field = self
                .field(name)
                .ok_or_else(|| self.value_error(name, ValidationErrorKind::UnknownField))?;
            field
                .validate_value(value)
                .map_err(|kind| self.value_error(name, kind))?;
        }
        Ok(())
    }

    /// Apply a validated partial update to an existing complete logical row.
    ///
    /// Both inputs are checked before the first modification, so a validation
    /// error leaves the row unchanged. Supplied values are moved into the row.
    /// This is an in-memory operation; target existence, authorization, storage
    /// transactions, and concurrency checks belong to the future runtime.
    pub fn apply_update(
        &self,
        row: &mut RecordValues,
        changes: RecordValues,
    ) -> Result<(), ValidationError> {
        self.validate_row(row)?;
        self.validate_update(&changes)?;
        row.extend(changes);
        Ok(())
    }

    fn resolve_insert_key_action(
        &self,
        values: &RecordValues,
    ) -> Result<InsertKeyAction, ValidationError> {
        let key = &self.primary_key().name;
        let action = if values.contains_key(key) {
            if self.insert_key_policy() == InsertKeyPolicy::GeneratedOnly {
                return Err(self.value_error(key, ValidationErrorKind::ExplicitKeyForbidden));
            }
            InsertKeyAction::Provided
        } else if self.key_generation() != &KeyGeneration::None {
            InsertKeyAction::Generate
        } else {
            return Err(self.value_error(key, ValidationErrorKind::MissingField));
        };

        Ok(action)
    }

    fn validate_insert_values(
        &self,
        values: &RecordValues,
        action: InsertKeyAction,
    ) -> Result<Vec<(&str, &Value)>, ValidationError> {
        let omitted_key = match action {
            InsertKeyAction::Provided => None,
            InsertKeyAction::Generate => Some(self.primary_key().name.as_str()),
        };
        self.validate_declared_values(values, omitted_key, true)
    }

    /// Locate an extra field after all expected supplied values have validated.
    fn reject_unknown_fields(&self, values: &RecordValues) -> Result<(), ValidationError> {
        for name in values.keys() {
            if self.field(name).is_none() {
                return Err(self.value_error(name, ValidationErrorKind::UnknownField));
            }
        }
        Ok(())
    }

    fn validate_declared_values(
        &self,
        values: &RecordValues,
        omitted_key: Option<&str>,
        allow_defaults: bool,
    ) -> Result<Vec<(&str, &Value)>, ValidationError> {
        let mut defaults = Vec::new();
        for field in self.fields() {
            if omitted_key == Some(field.name.as_str()) {
                continue;
            }
            let Some(value) = values.get(&field.name) else {
                if allow_defaults && let Some(default) = &field.default {
                    // Normalization already validated this immutable default.
                    defaults.push((field.name.as_str(), default));
                    continue;
                }
                return Err(self.value_error(&field.name, ValidationErrorKind::MissingField));
            };
            field
                .validate_value(value)
                .map_err(|kind| self.value_error(&field.name, kind))?;
        }
        // Every expected field was present and valid. Since map names are unique,
        // any remaining entries must be unknown. Valid inputs need no name scan.
        let expected_value_count =
            self.fields().len() - usize::from(omitted_key.is_some()) - defaults.len();
        if values.len() > expected_value_count {
            self.reject_unknown_fields(values)?;
        }
        Ok(defaults)
    }

    fn value_error(&self, field: &str, kind: ValidationErrorKind) -> ValidationError {
        ValidationError {
            table: self.name().to_owned(),
            field: field.to_owned(),
            kind,
        }
    }
}
