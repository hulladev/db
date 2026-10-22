use std::collections::BTreeMap;

use crate::{
    FieldDefinition, InsertKeyPolicy, KeyGeneration, ScalarType, SchemaError,
    UniqueConstraintContract, UniqueConstraintDeclaration, UuidGeneration,
};

/// A table description emitted by an authoring frontend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDeclaration {
    pub indexes: Vec<crate::IndexDeclaration>,
    pub checks: Vec<crate::CheckDefinition>,
    pub name: String,
    pub fields: Vec<FieldDefinition>,
    /// `None` selects `id`; an explicit name selects exactly one declared field.
    /// Selecting `id` explicitly also supports implicit ID injection.
    /// Composite primary keys are not represented by this initial contract.
    pub primary_key: Option<String>,
    /// `None` inherits the schema UUID default for UUID keys, or uses no
    /// generator for other types. Standalone normalization defaults to UUIDv7.
    /// `Some(KeyGeneration::None)` explicitly disables generation.
    pub key_generation: Option<KeyGeneration>,
    pub insert_key_policy: InsertKeyPolicy,
    /// Business uniqueness, independent of the selected primary key.
    pub unique_constraints: Vec<UniqueConstraintDeclaration>,
}

/// A structurally valid table with a resolved, non-null primary-key field.
///
/// Instances are constructed through [`TableDeclaration::normalize`]. Additional
/// compiler stages must resolve handler registrations, capabilities, and stable identities
/// before a schema can be activated or used for storage/code generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableContract {
    indexes: Vec<crate::IndexDeclaration>,
    checks: Vec<crate::CheckDefinition>,
    name: String,
    fields: Vec<FieldDefinition>,
    field_indices: BTreeMap<String, usize>,
    primary_key_index: usize,
    key_generation: KeyGeneration,
    insert_key_policy: InsertKeyPolicy,
    unique_constraints: Vec<UniqueConstraintContract>,
}

impl TableDeclaration {
    /// Declare a table with default key selection, generation, and insert policy.
    pub fn new(name: impl Into<String>, fields: Vec<FieldDefinition>) -> Self {
        Self {
            indexes: Vec::new(),
            checks: Vec::new(),
            name: name.into(),
            fields,
            primary_key: None,
            key_generation: None,
            insert_key_policy: InsertKeyPolicy::default(),
            unique_constraints: Vec::new(),
        }
    }

    /// Resolve the primary key without removing or rewriting authored fields.
    ///
    /// Declared fields retain their order. An injected `id` is appended, making
    /// normalization deterministic without imposing a backend's physical layout.
    pub fn normalize(self) -> Result<TableContract, SchemaError> {
        self.normalize_with_uuid_generation(UuidGeneration::default())
    }

    pub(crate) fn normalize_with_uuid_generation(
        mut self,
        default_uuid_generation: UuidGeneration,
    ) -> Result<TableContract, SchemaError> {
        if self.name.is_empty() {
            return Err(SchemaError::EmptyTableName);
        }

        let mut field_indices = BTreeMap::new();
        for (index, field) in self.fields.iter().enumerate() {
            if !field.constraints.validate_definition(field) {
                return Err(SchemaError::InvalidConstraint {
                    table: self.name,
                    name: field.name.clone(),
                });
            }
            if match field.scalar_type {
                ScalarType::Timestamp | ScalarType::LocalTimestamp => {
                    !matches!(field.precision, Some(0..=6))
                }
                _ => field.precision.is_some(),
            } {
                return Err(SchemaError::InvalidTemporalPrecision {
                    table: self.name,
                    field: field.name.clone(),
                });
            }
            if field
                .json_shape
                .as_ref()
                .is_some_and(|shape| field.scalar_type != ScalarType::Json || !shape.is_bounded())
            {
                return Err(SchemaError::InvalidFieldShape {
                    table: self.name,
                    field: field.name.clone(),
                });
            }
            if field.name.is_empty() {
                return Err(SchemaError::EmptyFieldName { table: self.name });
            }
            if field_indices.insert(field.name.clone(), index).is_some() {
                return Err(SchemaError::DuplicateField {
                    table: self.name,
                    field: field.name.clone(),
                });
            }
        }

        let primary_key_name = self.primary_key.as_deref().unwrap_or("id");
        if primary_key_name.is_empty() {
            return Err(SchemaError::EmptyPrimaryKeyName { table: self.name });
        }
        let primary_key_index = match field_indices.get(primary_key_name).copied() {
            Some(index) => index,
            None if primary_key_name == "id" => {
                let index = self.fields.len();
                self.fields
                    .push(FieldDefinition::new("id", ScalarType::Uuid));
                field_indices.insert("id".into(), index);
                index
            }
            None => {
                return Err(SchemaError::MissingPrimaryKeyField {
                    table: self.name,
                    field: primary_key_name.to_owned(),
                });
            }
        };

        if self.fields[primary_key_index].nullable {
            return Err(SchemaError::NullablePrimaryKey {
                table: self.name,
                field: self.fields[primary_key_index].name.clone(),
            });
        }

        let key = &self.fields[primary_key_index];
        if !matches!(
            key.scalar_type,
            ScalarType::Uuid | ScalarType::String | ScalarType::Int64
        ) {
            return Err(SchemaError::UnsupportedPrimaryKeyType {
                table: self.name,
                field: key.name.clone(),
                scalar_type: key.scalar_type,
            });
        }

        for (index, field) in self.fields.iter().enumerate() {
            if let Some(default) = &field.default {
                if index == primary_key_index {
                    return Err(SchemaError::DefaultOnPrimaryKey {
                        table: self.name,
                        field: field.name.clone(),
                    });
                }
                if let Err(reason) = field.validate_value(default) {
                    return Err(SchemaError::InvalidFieldDefault {
                        table: self.name,
                        field: field.name.clone(),
                        reason,
                    });
                }
            }
        }

        let key = &self.fields[primary_key_index];
        let key_generation = self.key_generation.unwrap_or(match key.scalar_type {
            ScalarType::Uuid => default_uuid_generation.key_generation(),
            _ => KeyGeneration::None,
        });

        if let KeyGeneration::Custom { handler, .. } = &key_generation
            && handler.is_empty()
        {
            return Err(SchemaError::EmptyGeneratorHandler { table: self.name });
        }

        if let Some(output_type) = key_generation.output_type()
            && output_type != key.scalar_type
        {
            return Err(SchemaError::IncompatibleKeyGenerator {
                table: self.name,
                field: key.name.clone(),
                key_type: key.scalar_type,
                output_type,
            });
        }

        if self.insert_key_policy == InsertKeyPolicy::GeneratedOnly
            && key_generation == KeyGeneration::None
        {
            return Err(SchemaError::MissingKeyGenerator {
                table: self.name,
                field: key.name.clone(),
            });
        }

        let unique_constraints = crate::unique::normalize_unique_constraints(
            &self.name,
            self.unique_constraints,
            &self.fields,
            &field_indices,
        )?;

        let mut names = std::collections::BTreeSet::new();
        for index in &self.indexes {
            let selected: std::collections::BTreeSet<_> = index.fields.iter().collect();
            if index.name.is_empty()
                || !names.insert(index.name.clone())
                || index.fields.is_empty()
                || selected.len() != index.fields.len()
                || index.fields.iter().any(|f| !field_indices.contains_key(f))
            {
                return Err(SchemaError::InvalidConstraint {
                    table: self.name,
                    name: index.name.clone(),
                });
            }
        }
        for check in &self.checks {
            if check.name.is_empty()
                || !names.insert(check.name.clone())
                || check.parts.len() != check.operands.len() + 1
                || (check.operands.is_empty() && check.parts.iter().all(|p| p.trim().is_empty()))
                || check.parts.iter().any(|p| {
                    ["\0", ";", "--", "/*", "*/"]
                        .iter()
                        .any(|bad| p.contains(bad))
                })
                || check.operands.iter().any(|operand| match operand {
                    crate::CheckOperand::Field(f) => !field_indices.contains_key(f),
                    crate::CheckOperand::Value(crate::Value::String(s)) => s.contains('\0'),
                    crate::CheckOperand::Value(crate::Value::Json(_)) => true,
                    _ => false,
                })
            {
                return Err(SchemaError::InvalidConstraint {
                    table: self.name,
                    name: check.name.clone(),
                });
            }
        }
        Ok(TableContract {
            indexes: self.indexes,
            checks: self.checks,
            name: self.name,
            fields: self.fields,
            field_indices,
            primary_key_index,
            key_generation,
            insert_key_policy: self.insert_key_policy,
            unique_constraints,
        })
    }
}

impl TableContract {
    pub fn indexes(&self) -> &[crate::IndexDeclaration] {
        &self.indexes
    }
    pub fn checks(&self) -> &[crate::CheckDefinition] {
        &self.checks
    }
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn fields(&self) -> &[FieldDefinition] {
        &self.fields
    }

    pub fn field(&self, name: &str) -> Option<&FieldDefinition> {
        self.field_indices
            .get(name)
            .map(|index| &self.fields[*index])
    }

    pub fn primary_key(&self) -> &FieldDefinition {
        &self.fields[self.primary_key_index]
    }

    pub fn key_generation(&self) -> &KeyGeneration {
        &self.key_generation
    }

    pub fn insert_key_policy(&self) -> InsertKeyPolicy {
        self.insert_key_policy
    }

    pub fn unique_constraints(&self) -> &[UniqueConstraintContract] {
        &self.unique_constraints
    }
}
