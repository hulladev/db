//! Decode the same canonical descriptor emitted by the TypeScript schema compiler.
use crate::{
    CheckDefinition, CheckOperand, FieldConstraints, FieldDefinition, IndexDeclaration,
    InsertKeyPolicy, ReferenceDeclaration, ReferenceOnDelete, ScalarType, SchemaDeclaration,
    TableDeclaration, UniqueConstraintDeclaration, UniqueNullPolicy, UuidGeneration, Value,
};
use crate::{KeyGeneration, SchemaContract};
use serde::Deserialize;
use serde_json::{Value as Json, json};

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
    json_shape: Option<crate::JsonShape>,
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
fn invalid() -> DescriptorError {
    DescriptorError::InvalidInput("invalid database schema descriptor".into())
}
fn scalar(value: &str) -> Result<ScalarType, DescriptorError> {
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
pub fn schema_from_descriptor(descriptor: Json) -> Result<SchemaContract, DescriptorError> {
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
            None if table
                .generation
                .as_object()
                .is_some_and(|fields| fields.len() == 2) =>
            {
                KeyGeneration::Custom {
                    handler: table.generation["custom"]
                        .as_str()
                        .ok_or_else(invalid)?
                        .into(),
                    output_type: scalar(table.generation["output"].as_str().ok_or_else(invalid)?)?,
                }
            }
            _ => return Err(invalid()),
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
                        .collect::<Result<_, DescriptorError>>()?,
                })
            })
            .collect::<Result<_, DescriptorError>>()?;
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
        .map_err(|error| DescriptorError::InvalidInput(error.to_string()))?;
    if !descriptor_equal(&contract.descriptor(), &descriptor) {
        return Err(invalid());
    }
    Ok(contract)
}

fn decode_value(value: Json) -> Result<Value, DescriptorError> {
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
        [kind, value] if kind == "int64" => Ok(Value::Int64(canonical_int64(value)?)),
        [kind, value] if kind == "uuid" => Ok(Value::Uuid(
            *uuid::Uuid::parse_str(value.as_str().ok_or_else(invalid)?)
                .map_err(|_| invalid())?
                .as_bytes(),
        )),
        _ => Err(invalid()),
    }
}

#[derive(Debug, Clone)]
pub enum DescriptorError {
    InvalidInput(String),
}
impl std::fmt::Display for DescriptorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for DescriptorError {}
fn canonical_int64(value: &Json) -> Result<i64, DescriptorError> {
    let text = value.as_str().ok_or_else(invalid)?;
    let number = text.parse::<i64>().map_err(|_| invalid())?;
    if number.to_string() != text {
        return Err(invalid());
    }
    Ok(number)
}

impl SchemaContract {
    pub fn descriptor(&self) -> Json {
        json!({
            "uuid_default": format!("{:?}", self.default_uuid_generation()),
            "tables": self.tables().iter().map(|table| json!({
                "name": table.name(),
                "key": table.primary_key().name,
                "generation": match table.key_generation() {
                    KeyGeneration::Custom { handler, output_type } => json!({"custom": handler, "output": format!("{output_type:?}")}),
                    other => json!(format!("{other:?}")),
                },
                "insert_policy": format!("{:?}", table.insert_key_policy()),
                "fields": table.fields().iter().map(|f| { let mut field = json!({
                    "name": f.name, "scalar": format!("{:?}", f.scalar_type), "nullable": f.nullable,
                    "default": f.default.as_ref().map(describe_value),
                    "constraints": describe_constraints(&f.constraints),
                }); if let Some(precision) = f.precision { field["precision"] = json!(precision); }
                if let Some(shape) = &f.json_shape { field["jsonShape"] = json!(shape); } field }).collect::<Vec<_>>(),
                "indexes": table.indexes().iter().map(|index| json!({"name":index.name,"fields":index.fields})).collect::<Vec<_>>(),
                "checks": table.checks().iter().map(|check| json!({"name":check.name,"parts":check.parts,"operands":check.operands.iter().map(|operand|match operand { CheckOperand::Field(field)=>json!({"field":field}),CheckOperand::Value(value)=>json!({"value":describe_value(value)}) }).collect::<Vec<_>>() })).collect::<Vec<_>>(),
                "unique": table.unique_constraints().iter().map(|u| json!({
                    "name": u.name(), "fields": u.fields(), "nulls": format!("{:?}", u.null_policy()),
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "references": self.references().iter().map(|r| json!({
                "table": r.table(), "field": r.field(), "target": r.target_table(), "key": r.target_field(), "onDelete":format!("{:?}",r.on_delete()),
            })).collect::<Vec<_>>(),
        })
    }
}
fn describe_value(value: &Value) -> Json {
    match value {
        Value::Json(value) => json!(["json", value]),
        Value::Float64(value) => json!(["float64", value]),
        Value::Null => json!(["null"]),
        Value::String(value) => json!(["string", value]),
        Value::Date(value) => json!(["date", value]),
        Value::Timestamp(value) => json!(["timestamp", value]),
        Value::LocalTimestamp(value) => json!(["localTimestamp", value]),
        Value::Boolean(value) => json!(["boolean", value]),
        Value::Int64(value) => json!(["int64", value.to_string()]),
        Value::Uuid(value) => json!(["uuid", uuid::Uuid::from_bytes(*value).to_string()]),
    }
}

fn describe_constraints(constraints: &crate::FieldConstraints) -> Json {
    let mut result =
        json!({"oneOf":constraints.one_of.iter().map(describe_value).collect::<Vec<_>>()});
    if let Some(min) = &constraints.min {
        result["min"] = describe_value(min);
    }
    if let Some(max) = &constraints.max {
        result["max"] = describe_value(max);
    }
    result
}

/// JSON preserves integer-vs-float syntax, but Float64 has one storage meaning.
/// Keep all other values strict, particularly arbitrary JSON and int64 text.
pub fn descriptor_equal(left: &Json, right: &Json) -> bool {
    match (left, right) {
        (Json::Array(left), Json::Array(right))
            if left.first().is_some_and(|tag| tag == "json")
                && right.first().is_some_and(|tag| tag == "json") =>
        {
            left == right
        }
        (Json::Array(left), Json::Array(right))
            if left.len() == 2
                && right.len() == 2
                && left[0] == "float64"
                && right[0] == "float64"
                && left[1].is_number()
                && right[1].is_number() =>
        {
            left[1]
                .as_f64()
                .zip(right[1].as_f64())
                .is_some_and(|(left, right)| left == right)
        }
        (Json::Array(left), Json::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| descriptor_equal(left, right))
        }
        (Json::Object(left), Json::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, left)| {
                    right
                        .get(key)
                        .is_some_and(|right| descriptor_equal(left, right))
                })
        }
        _ => left == right,
    }
}
