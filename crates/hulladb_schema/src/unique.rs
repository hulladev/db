use std::collections::{BTreeMap, BTreeSet};

use crate::{FieldDefinition, SchemaError};

/// How null participates in a unique constraint's tuple comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UniqueNullPolicy {
    /// A tuple containing any null does not conflict with another tuple.
    Distinct,
    /// Null compares equal to null; all tuple components must match to conflict.
    NotDistinct,
}

/// A named single-field or multi-field business uniqueness requirement.
///
/// Names are scoped to the table, not physical database constraint names or
/// stable migration identities. Storage enforcement is a later adapter stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UniqueConstraintDeclaration {
    pub name: String,
    pub fields: Vec<String>,
    /// Required when any selected field is nullable. For non-null fields,
    /// omission resolves to `NotDistinct`; either policy has the same effect.
    pub null_policy: Option<UniqueNullPolicy>,
}

impl UniqueConstraintDeclaration {
    pub fn new(name: impl Into<String>, fields: Vec<String>) -> Self {
        Self {
            name: name.into(),
            fields,
            null_policy: None,
        }
    }
}

/// A structurally validated uniqueness descriptor with explicit null semantics.
///
/// Field order is preserved for adapter planning; uniqueness compares the whole
/// tuple. Row validation alone cannot prove this cross-row requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UniqueConstraintContract {
    name: String,
    fields: Vec<String>,
    null_policy: UniqueNullPolicy,
}

impl UniqueConstraintContract {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn fields(&self) -> &[String] {
        &self.fields
    }

    pub fn null_policy(&self) -> UniqueNullPolicy {
        self.null_policy
    }
}

pub(crate) fn normalize_unique_constraints(
    table: &str,
    declarations: Vec<UniqueConstraintDeclaration>,
    fields: &[FieldDefinition],
    field_indices: &BTreeMap<String, usize>,
) -> Result<Vec<UniqueConstraintContract>, SchemaError> {
    let mut names = BTreeSet::new();
    let mut field_sets = BTreeMap::new();
    let mut constraints = Vec::with_capacity(declarations.len());
    for declaration in declarations {
        if declaration.name.is_empty() {
            return Err(SchemaError::EmptyUniqueConstraintName {
                table: table.into(),
            });
        }
        if !names.insert(declaration.name.clone()) {
            return Err(SchemaError::DuplicateUniqueConstraintName {
                table: table.into(),
                constraint: declaration.name,
            });
        }
        if declaration.fields.is_empty() {
            return Err(SchemaError::EmptyUniqueConstraintFields {
                table: table.into(),
                constraint: declaration.name,
            });
        }

        let mut indices = BTreeSet::new();
        let mut nullable = false;
        for field in &declaration.fields {
            let Some(&index) = field_indices.get(field) else {
                return Err(SchemaError::MissingUniqueConstraintField {
                    table: table.into(),
                    constraint: declaration.name,
                    field: field.clone(),
                });
            };
            if !indices.insert(index) {
                return Err(SchemaError::DuplicateUniqueConstraintField {
                    table: table.into(),
                    constraint: declaration.name,
                    field: field.clone(),
                });
            }
            nullable |= fields[index].nullable;
        }
        let null_policy = match declaration.null_policy {
            Some(policy) => policy,
            None if nullable => {
                return Err(SchemaError::MissingUniqueNullPolicy {
                    table: table.into(),
                    constraint: declaration.name,
                });
            }
            None => UniqueNullPolicy::NotDistinct,
        };
        let signature: Vec<_> = indices.into_iter().collect();
        if let Some(existing_constraint) = field_sets.insert(signature, declaration.name.clone()) {
            return Err(SchemaError::DuplicateUniqueConstraint {
                table: table.into(),
                constraint: declaration.name,
                existing_constraint,
            });
        }
        constraints.push(UniqueConstraintContract {
            name: declaration.name,
            fields: declaration.fields,
            null_policy,
        });
    }
    Ok(constraints)
}
