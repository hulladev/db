use std::{collections::BTreeMap, fmt};

use crate::{FieldDefinition, RecordValues, TableContract, ValidationError, ValidationErrorKind};

/// An explicit entity projection, distinct from a complete table or an update.
/// Fields preserve selection order and nullability; insert defaults do not apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionContract {
    name: String,
    table: String,
    fields: Vec<FieldDefinition>,
    indices: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionError {
    pub table: String,
    pub projection: String,
    pub kind: ProjectionErrorKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionErrorKind {
    EmptyName,
    EmptySelection,
    UnknownField(String),
    DuplicateField(String),
    MissingPrimaryKey(String),
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "table {:?}, projection {:?}: {:?}",
            self.table, self.projection, self.kind
        )
    }
}

impl std::error::Error for ProjectionError {}

impl TableContract {
    /// Resolve an explicit, nonempty entity projection including its primary key.
    /// The normalizer never adds newly declared table fields to this selection.
    pub fn projection(
        &self,
        name: &str,
        selected: &[&str],
    ) -> Result<ProjectionContract, ProjectionError> {
        let error = |kind| ProjectionError {
            table: self.name().into(),
            projection: name.into(),
            kind,
        };
        if name.is_empty() {
            return Err(error(ProjectionErrorKind::EmptyName));
        }
        if selected.is_empty() {
            return Err(error(ProjectionErrorKind::EmptySelection));
        }
        let mut fields = Vec::with_capacity(selected.len());
        let mut indices = BTreeMap::new();
        for field in selected {
            let mut definition = self
                .field(field)
                .cloned()
                .ok_or_else(|| error(ProjectionErrorKind::UnknownField((*field).into())))?;
            if indices.insert((*field).into(), fields.len()).is_some() {
                return Err(error(ProjectionErrorKind::DuplicateField((*field).into())));
            }
            definition.default = None;
            fields.push(definition);
        }
        let key = &self.primary_key().name;
        if !indices.contains_key(key) {
            return Err(error(ProjectionErrorKind::MissingPrimaryKey(key.clone())));
        }
        Ok(ProjectionContract {
            name: name.into(),
            table: self.name().into(),
            fields,
            indices,
        })
    }
}

impl ProjectionContract {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn table(&self) -> &str {
        &self.table
    }
    pub fn fields(&self) -> &[FieldDefinition] {
        &self.fields
    }
    pub fn field(&self, name: &str) -> Option<&FieldDefinition> {
        self.indices.get(name).map(|index| &self.fields[*index])
    }

    /// Every selected field must be present, including nullable/defaulted fields.
    /// Unselected fields are rejected; null remains distinct from omission.
    pub fn validate_record(&self, values: &RecordValues) -> Result<(), ValidationError> {
        let error = |field: &str, kind| ValidationError {
            table: self.table.clone(),
            field: field.into(),
            kind,
        };
        for field in &self.fields {
            let value = values
                .get(&field.name)
                .ok_or_else(|| error(&field.name, ValidationErrorKind::MissingField))?;
            field
                .validate_value(value)
                .map_err(|kind| error(&field.name, kind))?;
        }
        for name in values.keys() {
            if !self.indices.contains_key(name) {
                return Err(error(name, ValidationErrorKind::UnknownField));
            }
        }
        Ok(())
    }
}
