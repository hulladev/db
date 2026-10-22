//! Decode the same canonical descriptor emitted by the TypeScript schema compiler.
use super::super::{Error, ManagedSchema};
use hulladb_schema::{
    CheckDefinition, CheckOperand, FieldConstraints, FieldDefinition, IndexDeclaration,
    InsertKeyPolicy, KeyGeneration, ReferenceDeclaration, ReferenceOnDelete, ScalarType,
    SchemaDeclaration, TableDeclaration, UniqueConstraintDeclaration, UniqueNullPolicy,
    UuidGeneration, Value,
};
use serde::Deserialize;
use serde_json::Value as Json;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Descriptor {
    uuid_default: String,
    tables: Vec<Table>,
    references: Vec<Reference>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    name: String,
    key: String,
    generation: Json,
    insert_policy: String,
    fields: Vec<Field>,
    unique: Vec<Unique>,
    indexes: Vec<Index>,
    checks: Vec<Check>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Field {
    name: String,
    scalar: String,
    nullable: bool,
    precision: Option<u8>,
    default: Option<Json>,
    constraints: Constraints,
    #[serde(rename = "jsonShape")]
    json_shape: Option<hulladb_schema::JsonShape>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Constraints {
    min: Option<Json>,
    max: Option<Json>,
    #[serde(rename = "oneOf")]
    one_of: Vec<Json>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    name: String,
    fields: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Check {
    name: String,
    parts: Vec<String>,
    operands: Vec<Operand>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum Operand {
    Field { field: String },
    Value { value: Json },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Unique {
    name: String,
    fields: Vec<String>,
    nulls: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    table: String,
    field: String,
    target: String,
    key: String,
    #[serde(rename = "onDelete")]
    on_delete: String,
}
fn invalid() -> Error {
    Error::InvalidInput("invalid database schema descriptor".into())
}
fn scalar(value: &str) -> Result<ScalarType, Error> {
    match value {
        "Date" => Ok(ScalarType::Date),
        "Timestamp" => Ok(ScalarType::Timestamp),
        "LocalTimestamp" => Ok(ScalarType::LocalTimestamp),
        "Json" => Ok(ScalarType::Json),
        "Float64" => Ok(ScalarType::Float64),
        "String" => Ok(ScalarType::String),
        "Boolean" => Ok(ScalarType::Boolean),
        "Int64" => Ok(ScalarType::Int64),
        "Uuid" => Ok(ScalarType::Uuid),
        _ => Err(invalid()),
    }
}

#[doc(hidden)]
pub fn schema_from_descriptor(namespace: &str, descriptor: Json) -> Result<ManagedSchema, Error> {
    let parsed: Descriptor = serde_json::from_value(descriptor.clone()).map_err(|_| invalid())?;
    let mut declaration = SchemaDeclaration::new(Vec::new());
    declaration.default_uuid_generation = match parsed.uuid_default.as_str() {
        "V4" => UuidGeneration::V4,
        "V7" => UuidGeneration::V7,
        _ => return Err(invalid()),
    };
    for table in parsed.tables {
        let mut fields = Vec::new();
        for field in table.fields {
            let mut definition = FieldDefinition::new(field.name, scalar(&field.scalar)?);
            definition.nullable = field.nullable;
            definition.precision = field.precision;
            definition.json_shape = field.json_shape;
            definition.default = field.default.map(decode_value).transpose()?;
            definition.constraints = FieldConstraints {
                min: field.constraints.min.map(decode_value).transpose()?,
                max: field.constraints.max.map(decode_value).transpose()?,
                one_of: field
                    .constraints
                    .one_of
                    .into_iter()
                    .map(decode_value)
                    .collect::<Result<_, _>>()?,
            };
            fields.push(definition);
        }
        let mut definition = TableDeclaration::new(table.name, fields);
        definition.primary_key = Some(table.key);
        definition.insert_key_policy = match table.insert_policy.as_str() {
            "AllowExplicit" => InsertKeyPolicy::AllowExplicit,
            "GeneratedOnly" => InsertKeyPolicy::GeneratedOnly,
            _ => return Err(invalid()),
        };
        definition.key_generation = Some(match table.generation.as_str() {
            Some("None") => KeyGeneration::None,
            Some("UuidV4") => KeyGeneration::UuidV4,
            Some("UuidV7") => KeyGeneration::UuidV7,
            // Custom generators require executable registration unavailable to direct server access.
            _ => {
                return Err(Error::InvalidInput(
                    "direct database access does not support custom key generators".into(),
                ));
            }
        });
        for unique in table.unique {
            definition
                .unique_constraints
                .push(UniqueConstraintDeclaration {
                    name: unique.name,
                    fields: unique.fields,
                    null_policy: Some(match unique.nulls.as_str() {
                        "Distinct" => UniqueNullPolicy::Distinct,
                        "NotDistinct" => UniqueNullPolicy::NotDistinct,
                        _ => return Err(invalid()),
                    }),
                });
        }
        definition.indexes = table
            .indexes
            .into_iter()
            .map(|i| IndexDeclaration {
                name: i.name,
                fields: i.fields,
            })
            .collect();
        definition.checks = table
            .checks
            .into_iter()
            .map(|c| {
                Ok(CheckDefinition {
                    name: c.name,
                    parts: c.parts,
                    operands: c
                        .operands
                        .into_iter()
                        .map(|operand| match operand {
                            Operand::Field { field } => Ok(CheckOperand::Field(field)),
                            Operand::Value { value } => {
                                Ok(CheckOperand::Value(decode_value(value)?))
                            }
                        })
                        .collect::<Result<_, Error>>()?,
                })
            })
            .collect::<Result<_, Error>>()?;
        declaration.tables.push(definition);
    }
    for reference in parsed.references {
        // The normalized descriptor comparison below also verifies the target key.
        let _ = reference.key;
        let mut definition =
            ReferenceDeclaration::new(reference.table, reference.field, reference.target);
        definition.on_delete = match reference.on_delete.as_str() {
            "Restrict" => ReferenceOnDelete::Restrict,
            "Cascade" => ReferenceOnDelete::Cascade,
            "SetNull" => ReferenceOnDelete::SetNull,
            _ => return Err(invalid()),
        };
        declaration.references.push(definition);
    }
    let contract = declaration
        .normalize()
        .map_err(|error| Error::InvalidInput(error.to_string()))?;
    let schema = ManagedSchema::new(namespace, contract)?;
    if !super::super::storage::descriptor_equal(&schema.descriptor(), &descriptor) {
        return Err(invalid());
    }
    Ok(schema)
}

fn decode_value(value: Json) -> Result<Value, Error> {
    let values = value.as_array().ok_or_else(invalid)?;
    match values.as_slice() {
        [kind, value] if kind == "json" => Ok(Value::Json(value.clone())),
        [kind, Json::Number(value)] if kind == "float64" => Ok(Value::Float64(value.clone())),
        [kind] if kind == "null" => Ok(Value::Null),
        [kind, Json::String(value)] if kind == "date" => Ok(Value::Date(value.clone())),
        [kind, Json::String(value)] if kind == "timestamp" => Ok(Value::Timestamp(value.clone())),
        [kind, Json::String(value)] if kind == "localTimestamp" => {
            Ok(Value::LocalTimestamp(value.clone()))
        }
        [kind, value] if kind == "string" => {
            Ok(Value::String(value.as_str().ok_or_else(invalid)?.into()))
        }
        [kind, value] if kind == "boolean" => {
            Ok(Value::Boolean(value.as_bool().ok_or_else(invalid)?))
        }
        [kind, value] if kind == "int64" => Ok(Value::Int64(super::contract::int64(value)?)),
        [kind, value] if kind == "uuid" => Ok(Value::Uuid(
            *uuid::Uuid::parse_str(value.as_str().ok_or_else(invalid)?)
                .map_err(|_| invalid())?
                .as_bytes(),
        )),
        _ => Err(invalid()),
    }
}
