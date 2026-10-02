use std::fmt;

use crate::{ScalarType, ValidationErrorKind};

/// Structural, default-value, key, uniqueness, and reference normalization errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaError {
    EmptyUniqueConstraintName {
        table: String,
    },
    DuplicateUniqueConstraintName {
        table: String,
        constraint: String,
    },
    EmptyUniqueConstraintFields {
        table: String,
        constraint: String,
    },
    MissingUniqueConstraintField {
        table: String,
        constraint: String,
        field: String,
    },
    DuplicateUniqueConstraintField {
        table: String,
        constraint: String,
        field: String,
    },
    MissingUniqueNullPolicy {
        table: String,
        constraint: String,
    },
    DuplicateUniqueConstraint {
        table: String,
        constraint: String,
        existing_constraint: String,
    },
    DuplicateTable {
        table: String,
    },
    MissingReferenceSourceTable {
        table: String,
        field: String,
    },
    MissingReferenceSourceField {
        table: String,
        field: String,
    },
    DuplicateReference {
        table: String,
        field: String,
    },
    MissingReferenceTargetTable {
        table: String,
        field: String,
        target_table: String,
    },
    IncompatibleReferenceType {
        table: String,
        field: String,
        target_table: String,
        field_type: ScalarType,
        key_type: ScalarType,
    },
    EmptyTableName,
    EmptyFieldName {
        table: String,
    },
    EmptyPrimaryKeyName {
        table: String,
    },
    DuplicateField {
        table: String,
        field: String,
    },
    MissingPrimaryKeyField {
        table: String,
        field: String,
    },
    NullablePrimaryKey {
        table: String,
        field: String,
    },
    UnsupportedPrimaryKeyType {
        table: String,
        field: String,
        scalar_type: ScalarType,
    },
    EmptyGeneratorHandler {
        table: String,
    },
    IncompatibleKeyGenerator {
        table: String,
        field: String,
        key_type: ScalarType,
        output_type: ScalarType,
    },
    MissingKeyGenerator {
        table: String,
        field: String,
    },
    DefaultOnPrimaryKey {
        table: String,
        field: String,
    },
    InvalidFieldDefault {
        table: String,
        field: String,
        reason: ValidationErrorKind,
    },
}

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyUniqueConstraintName { table } => write!(
                f,
                "table {table:?} declares a unique constraint with an empty name"
            ),
            Self::DuplicateUniqueConstraintName { table, constraint } => write!(
                f,
                "table {table:?} declares unique constraint name {constraint:?} more than once"
            ),
            Self::EmptyUniqueConstraintFields { table, constraint } => write!(
                f,
                "table {table:?} unique constraint {constraint:?} must select at least one field"
            ),
            Self::MissingUniqueConstraintField {
                table,
                constraint,
                field,
            } => write!(
                f,
                "table {table:?} unique constraint {constraint:?} selects undeclared field {field:?}"
            ),
            Self::DuplicateUniqueConstraintField {
                table,
                constraint,
                field,
            } => write!(
                f,
                "table {table:?} unique constraint {constraint:?} selects field {field:?} more than once"
            ),
            Self::MissingUniqueNullPolicy { table, constraint } => write!(
                f,
                "table {table:?} unique constraint {constraint:?} includes a nullable field; explicitly choose Distinct or NotDistinct null semantics"
            ),
            Self::DuplicateUniqueConstraint {
                table,
                constraint,
                existing_constraint,
            } => write!(
                f,
                "table {table:?} unique constraint {constraint:?} selects the same fields as {existing_constraint:?}; keep one declaration"
            ),
            Self::DuplicateTable { table } => {
                write!(f, "table {table:?} is declared more than once")
            }
            Self::MissingReferenceSourceTable { table, field } => write!(
                f,
                "reference from {table:?}.{field:?} selects an undeclared source table"
            ),
            Self::MissingReferenceSourceField { table, field } => write!(
                f,
                "table {table:?} references from undeclared field {field:?}"
            ),
            Self::DuplicateReference { table, field } => write!(
                f,
                "table {table:?} field {field:?} has more than one reference declaration"
            ),
            Self::MissingReferenceTargetTable {
                table,
                field,
                target_table,
            } => write!(
                f,
                "table {table:?} field {field:?} references undeclared table {target_table:?}"
            ),
            Self::IncompatibleReferenceType {
                table,
                field,
                target_table,
                field_type,
                key_type,
            } => write!(
                f,
                "table {table:?} field {field:?} has type {field_type:?}, but references table {target_table:?} with key type {key_type:?}"
            ),
            Self::EmptyTableName => write!(f, "a table name must not be empty"),
            Self::EmptyFieldName { table } => {
                write!(f, "table {table:?} contains a field with an empty name")
            }
            Self::EmptyPrimaryKeyName { table } => {
                write!(f, "table {table:?} selects an empty primary-key field name")
            }
            Self::DuplicateField { table, field } => {
                write!(f, "table {table:?} declares field {field:?} more than once")
            }
            Self::MissingPrimaryKeyField { table, field } => write!(
                f,
                "table {table:?} selects undeclared primary-key field {field:?}; declare that field"
            ),
            Self::NullablePrimaryKey { table, field } => write!(
                f,
                "table {table:?} selects nullable primary-key field {field:?}; make the field non-null"
            ),
            Self::UnsupportedPrimaryKeyType {
                table,
                field,
                scalar_type,
            } => write!(
                f,
                "table {table:?} primary key {field:?} has unsupported type {scalar_type:?}; use UUID, string, or signed 64-bit integer"
            ),
            Self::EmptyGeneratorHandler { table } => write!(
                f,
                "table {table:?} references an empty key generator handler name"
            ),
            Self::IncompatibleKeyGenerator {
                table,
                field,
                key_type,
                output_type,
            } => write!(
                f,
                "table {table:?} key {field:?} has type {key_type:?}, but its generator produces {output_type:?}; configure a compatible generator"
            ),
            Self::MissingKeyGenerator { table, field } => write!(
                f,
                "table {table:?} requires generated keys for {field:?} but has no generator; configure one or allow explicit keys"
            ),
            Self::DefaultOnPrimaryKey { table, field } => write!(
                f,
                "table {table:?} primary key {field:?} has a field default; configure key generation instead"
            ),
            Self::InvalidFieldDefault {
                table,
                field,
                reason,
            } => write!(
                f,
                "table {table:?} field {field:?} has an invalid default: {reason:?}"
            ),
        }
    }
}

impl std::error::Error for SchemaError {}
