use std::collections::BTreeMap;

use hulladb_schema::{FieldDefinition, ScalarType, TableContract, Value};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use super::{Error, ManagedSchema};

/// A database assignment expression; references never contain caller SQL.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum WriteExpression {
    Value { value: Json },
    Field { source: WriteSource, field: String },
    Add { left: Box<Self>, right: Box<Self> },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WriteSource {
    Current,
    Incoming,
}

pub type WriteValues = BTreeMap<String, WriteExpression>;

impl WriteExpression {
    pub(super) fn literal(value: Value) -> Self {
        let value = match value {
            Value::Null => Json::Null,
            Value::String(value)
            | Value::Date(value)
            | Value::Timestamp(value)
            | Value::LocalTimestamp(value) => Json::String(value),
            Value::Boolean(value) => Json::Bool(value),
            Value::Int64(value) => Json::String(value.to_string()),
            Value::Float64(value) => Json::Number(value),
            Value::Uuid(value) => Json::String(Uuid::from_bytes(value).to_string()),
            Value::Json(value) => value,
        };
        Self::Value { value }
    }
}

pub(super) fn assignments(
    schema: &ManagedSchema,
    table: &TableContract,
    values: &WriteValues,
    incoming: bool,
    parameters: &mut Vec<Box<dyn ToSql + Sync + Send>>,
) -> Result<Vec<String>, Error> {
    if values.is_empty() {
        return Err(invalid("updates require changes"));
    }
    let mut pending: Vec<_> = values.values().map(|expression| (expression, 0)).collect();
    let mut nodes = 0;
    while let Some((expression, depth)) = pending.pop() {
        nodes += 1;
        if nodes > 1024 || depth > 64 {
            return Err(invalid(
                "assignment expressions exceed the complexity limit",
            ));
        }
        if let WriteExpression::Add { left, right } = expression {
            pending.push((left, depth + 1));
            pending.push((right, depth + 1));
        }
    }
    values
        .iter()
        .map(|(name, expression)| {
            let field = table
                .field(name)
                .ok_or_else(|| invalid("unknown assignment field"))?;
            if name == &table.primary_key().name {
                return Err(invalid("primary key is immutable"));
            }
            let expression = compile(schema, table, field, expression, incoming, parameters, 0)?;
            Ok(format!("{}={expression}", schema.column(table, name)?))
        })
        .collect()
}

fn compile(
    schema: &ManagedSchema,
    table: &TableContract,
    target: &FieldDefinition,
    expression: &WriteExpression,
    incoming: bool,
    parameters: &mut Vec<Box<dyn ToSql + Sync + Send>>,
    depth: usize,
) -> Result<String, Error> {
    if depth > 64 {
        return Err(invalid("assignment expression is too deep"));
    }
    match expression {
        WriteExpression::Value { value } => {
            let value = decode(target, value)?;
            let mut operand = target.clone();
            if depth > 0 {
                operand.constraints = hulladb_schema::FieldConstraints::default();
            }
            operand
                .validate_value(&value)
                .map_err(|_| invalid("invalid assignment value"))?;
            parameters.push(super::db::parameter(target, &value)?);
            Ok(format!("${}", parameters.len()))
        }
        WriteExpression::Field { source, field } => {
            let field = table
                .field(field)
                .ok_or_else(|| invalid("unknown expression field"))?;
            if field.scalar_type != target.scalar_type || (!target.nullable && field.nullable) {
                return Err(invalid("incompatible assignment field"));
            }
            if field.precision > target.precision {
                return Err(invalid(
                    "assignment would silently discard timestamp precision",
                ));
            }
            if target.scalar_type == ScalarType::Json
                && target.json_shape.as_ref().is_some_and(|shape| {
                    !matches!(shape, hulladb_schema::JsonShape::Any)
                        && field.json_shape.as_ref() != Some(shape)
                })
            {
                return Err(invalid("incompatible JSON assignment shape"));
            }
            let alias = match source {
                WriteSource::Current => "current",
                WriteSource::Incoming if incoming => "excluded",
                WriteSource::Incoming => {
                    return Err(invalid("incoming is only available in conflict updates"));
                }
            };
            Ok(format!("{alias}.{}", schema.column(table, &field.name)?))
        }
        WriteExpression::Add { left, right } => {
            if !matches!(target.scalar_type, ScalarType::Int64 | ScalarType::Float64) {
                return Err(invalid("addition requires numeric fields"));
            }
            let left = compile(schema, table, target, left, incoming, parameters, depth + 1)?;
            let right = compile(
                schema,
                table,
                target,
                right,
                incoming,
                parameters,
                depth + 1,
            )?;
            // Explicit casts avoid PostgreSQL resolving two literal parameters to text.
            let scalar = if target.scalar_type == ScalarType::Int64 {
                "bigint"
            } else {
                "double precision"
            };
            Ok(format!("(({left})::{scalar}+({right})::{scalar})"))
        }
    }
}

fn decode(field: &FieldDefinition, value: &Json) -> Result<Value, Error> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    match field.scalar_type {
        ScalarType::Json => Some(Value::Json(value.clone())),
        ScalarType::Float64 => value.as_number().cloned().map(Value::Float64),
        ScalarType::Date => value.as_str().map(|v| Value::Date(v.into())),
        ScalarType::Timestamp => value.as_str().map(|v| Value::Timestamp(v.into())),
        ScalarType::LocalTimestamp => value.as_str().map(|v| Value::LocalTimestamp(v.into())),
        ScalarType::String => value.as_str().map(|v| Value::String(v.into())),
        ScalarType::Boolean => value.as_bool().map(Value::Boolean),
        ScalarType::Int64 => value
            .as_str()
            .and_then(|v| v.parse::<i64>().ok().filter(|n| n.to_string() == v))
            .map(Value::Int64),
        ScalarType::Uuid => value
            .as_str()
            .and_then(|v| Uuid::parse_str(v).ok())
            .map(|v| Value::Uuid(*v.as_bytes())),
    }
    .ok_or_else(|| invalid("invalid assignment value"))
}
fn invalid(message: &str) -> Error {
    Error::InvalidInput(message.into())
}
