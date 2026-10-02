use std::collections::{BTreeMap, BTreeSet};

use crate::{ScalarType, SchemaError, TableContract, TableDeclaration, UuidGeneration};

/// An explicit single-field reference to another table's selected primary key.
///
/// Storage must enforce target existence and restrict deletion of referenced rows;
/// automatic deletion or clearing of dependent rows is not part of this slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceDeclaration {
    pub table: String,
    pub field: String,
    pub target_table: String,
}

impl ReferenceDeclaration {
    pub fn new(
        table: impl Into<String>,
        field: impl Into<String>,
        target_table: impl Into<String>,
    ) -> Self {
        Self {
            table: table.into(),
            field: field.into(),
            target_table: target_table.into(),
        }
    }
}

/// Tables and explicit references emitted by an authoring frontend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaDeclaration {
    pub tables: Vec<TableDeclaration>,
    pub references: Vec<ReferenceDeclaration>,
    /// UUID fallback for tables without an explicit generator. Defaults to v7.
    pub default_uuid_generation: UuidGeneration,
}

/// A reference resolved against normalized tables. This is compiler metadata,
/// not evidence that any particular referenced row exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceContract {
    table: String,
    field: String,
    target_table: String,
    target_field: String,
    scalar_type: ScalarType,
    nullable: bool,
}

/// Structurally validated tables and references, with read-only access.
///
/// Storage adapters must enforce references within authoritative transactions.
/// Normalization alone does not make the schema deployable or guarantee ACID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaContract {
    tables: Vec<TableContract>,
    table_indices: BTreeMap<String, usize>,
    references: Vec<ReferenceContract>,
    default_uuid_generation: UuidGeneration,
}

impl SchemaDeclaration {
    pub fn new(tables: Vec<TableDeclaration>) -> Self {
        Self {
            tables,
            references: Vec::new(),
            default_uuid_generation: UuidGeneration::default(),
        }
    }

    /// Normalize all tables before resolving references, allowing forward,
    /// self, and cyclic references without depending on declaration order.
    ///
    /// Errors follow table declaration order, then reference declaration order.
    /// A source field may have at most one reference declaration.
    pub fn normalize(self) -> Result<SchemaContract, SchemaError> {
        let mut tables = Vec::with_capacity(self.tables.len());
        let mut table_indices = BTreeMap::new();
        for declaration in self.tables {
            let table = declaration.normalize_with_uuid_generation(self.default_uuid_generation)?;
            if table_indices
                .insert(table.name().to_owned(), tables.len())
                .is_some()
            {
                return Err(SchemaError::DuplicateTable {
                    table: table.name().to_owned(),
                });
            }
            tables.push(table);
        }

        let mut references = Vec::with_capacity(self.references.len());
        let mut sources = BTreeSet::new();
        for reference in self.references {
            let Some(&source_index) = table_indices.get(&reference.table) else {
                return Err(SchemaError::MissingReferenceSourceTable {
                    table: reference.table,
                    field: reference.field,
                });
            };
            let Some(field) = tables[source_index].field(&reference.field) else {
                return Err(SchemaError::MissingReferenceSourceField {
                    table: reference.table,
                    field: reference.field,
                });
            };
            if !sources.insert((source_index, reference.field.clone())) {
                return Err(SchemaError::DuplicateReference {
                    table: reference.table,
                    field: reference.field,
                });
            }
            let Some(&target_index) = table_indices.get(&reference.target_table) else {
                return Err(SchemaError::MissingReferenceTargetTable {
                    table: reference.table,
                    field: reference.field,
                    target_table: reference.target_table,
                });
            };
            let key = tables[target_index].primary_key();
            if field.scalar_type != key.scalar_type {
                return Err(SchemaError::IncompatibleReferenceType {
                    table: reference.table,
                    field: reference.field,
                    target_table: reference.target_table,
                    field_type: field.scalar_type,
                    key_type: key.scalar_type,
                });
            }
            references.push(ReferenceContract {
                table: reference.table,
                field: reference.field,
                target_table: reference.target_table,
                target_field: key.name.clone(),
                scalar_type: key.scalar_type,
                nullable: field.nullable,
            });
        }

        Ok(SchemaContract {
            tables,
            table_indices,
            references,
            default_uuid_generation: self.default_uuid_generation,
        })
    }
}

impl SchemaContract {
    pub fn default_uuid_generation(&self) -> UuidGeneration {
        self.default_uuid_generation
    }

    pub fn tables(&self) -> &[TableContract] {
        &self.tables
    }

    pub fn table(&self, name: &str) -> Option<&TableContract> {
        self.table_indices
            .get(name)
            .map(|index| &self.tables[*index])
    }

    pub fn references(&self) -> &[ReferenceContract] {
        &self.references
    }
}

impl ReferenceContract {
    pub fn table(&self) -> &str {
        &self.table
    }

    pub fn field(&self) -> &str {
        &self.field
    }

    pub fn target_table(&self) -> &str {
        &self.target_table
    }

    pub fn target_field(&self) -> &str {
        &self.target_field
    }

    pub fn scalar_type(&self) -> ScalarType {
        self.scalar_type
    }

    pub fn nullable(&self) -> bool {
        self.nullable
    }
}
