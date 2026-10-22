use super::db::parameter;
use super::{Error, ManagedSchema};
use hulladb_schema::{ScalarType, TableContract, Value};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::BTreeMap;
use tokio_postgres::types::ToSql;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Predicate {
    Constant {
        value: bool,
    },
    And {
        predicates: Vec<Predicate>,
    },
    Or {
        predicates: Vec<Predicate>,
    },
    Not {
        predicate: Box<Predicate>,
    },
    Compare {
        field: FieldPath,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transform: Option<String>,
        operator: String,
        operand: Operand,
        #[serde(rename = "caseSensitive")]
        case_sensitive: Option<bool>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Operand {
    Value {
        value: Json,
    },
    Field {
        field: FieldPath,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transform: Option<String>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Order {
    pub field: String,
    pub direction: Direction,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Asc,
    Desc,
}
pub(super) fn ordering(
    schema: &ManagedSchema,
    table: &TableContract,
    order: &[Order],
) -> Result<String, Error> {
    if order.len() > 64 {
        return Err(invalid());
    }
    let mut parts = Vec::new();
    for item in order {
        if table
            .field(&item.field)
            .is_none_or(|f| f.scalar_type == ScalarType::Json)
        {
            return Err(invalid());
        }
        parts.push(format!(
            "{} {}",
            schema.column(table, &item.field)?,
            match item.direction {
                Direction::Asc => "ASC",
                Direction::Desc => "DESC",
            }
        ));
    }
    if !order
        .iter()
        .any(|item| item.field == table.primary_key().name)
    {
        parts.push(format!(
            "{} {}",
            schema.column(table, &table.primary_key().name)?,
            match order
                .last()
                .map(|item| item.direction)
                .unwrap_or(Direction::Asc)
            {
                Direction::Asc => "ASC",
                Direction::Desc => "DESC",
            }
        ));
    }
    Ok(parts.join(","))
}
fn invalid() -> Error {
    Error::InvalidInput("invalid predicate or operand".into())
}
pub(super) fn compile(
    schema: &ManagedSchema,
    table: &TableContract,
    filters: &[Predicate],
    parameters: &mut Vec<Box<dyn ToSql + Sync + Send>>,
) -> Result<String, Error> {
    let mut budget = 1024;
    let mut parts = vec!["_scope=$1".to_owned()];
    for predicate in filters {
        parts.push(node(
            &Source::Table(schema, table),
            predicate,
            parameters,
            0,
            &mut budget,
        )?);
    }
    Ok(parts.join(" AND "))
}
/// Computed numeric results bind decimal text through PostgreSQL numeric, avoiding
/// both int64 truncation and lexical comparisons of lossless aggregate results.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum NumericKind {
    Integer,
    Decimal,
}
pub(super) struct OutputColumn {
    pub definition: hulladb_schema::FieldDefinition,
    pub expression: String,
    pub numeric: Option<NumericKind>,
}
enum Source<'a> {
    Table(&'a ManagedSchema, &'a TableContract),
    Outputs(&'a BTreeMap<String, OutputColumn>),
}
struct Resolved {
    definition: hulladb_schema::FieldDefinition,
    column: String,
    json_column: Option<String>,
    shape: Option<hulladb_schema::JsonShape>,
    numeric: Option<NumericKind>,
}
impl Source<'_> {
    fn resolve(
        &self,
        field: &FieldPath,
        transform: Option<&str>,
        params: &mut Vec<Box<dyn ToSql + Sync + Send>>,
    ) -> Result<Resolved, Error> {
        match self {
            Self::Table(schema, table) => {
                let (definition, column, json_column, shape) =
                    resolve(schema, table, field, transform, params)?;
                Ok(Resolved {
                    definition,
                    column,
                    json_column,
                    shape,
                    numeric: None,
                })
            }
            Self::Outputs(columns) => {
                let name = match field {
                    FieldPath::Name(name) => name,
                    FieldPath::Segments(parts) if parts.len() == 1 => &parts[0],
                    _ => return Err(invalid()),
                };
                if transform.is_some() {
                    return Err(invalid());
                }
                let value = columns.get(name).ok_or_else(invalid)?;
                Ok(Resolved {
                    definition: value.definition.clone(),
                    column: value.expression.clone(),
                    json_column: None,
                    shape: None,
                    numeric: value.numeric,
                })
            }
        }
    }
    fn is_nested(&self, field: &FieldPath) -> bool {
        matches!(self, Self::Table(_, table) if field.is_nested(table))
    }
}
pub(super) fn compile_outputs(
    columns: &BTreeMap<String, OutputColumn>,
    filters: &[Predicate],
    parameters: &mut Vec<Box<dyn ToSql + Sync + Send>>,
) -> Result<String, Error> {
    let mut budget = 1024;
    let mut parts = vec!["TRUE".to_owned()];
    for predicate in filters {
        parts.push(node(
            &Source::Outputs(columns),
            predicate,
            parameters,
            0,
            &mut budget,
        )?);
    }
    Ok(parts.join(" AND "))
}
fn node(
    source: &Source<'_>,
    predicate: &Predicate,
    params: &mut Vec<Box<dyn ToSql + Sync + Send>>,
    depth: usize,
    budget: &mut usize,
) -> Result<String, Error> {
    if depth > 32 || *budget == 0 {
        return Err(invalid());
    }
    *budget -= 1;
    Ok(match predicate {
        Predicate::Constant { value } => if *value { "TRUE" } else { "FALSE" }.into(),
        Predicate::Not { predicate } => format!(
            "(NOT {})",
            node(source, predicate, params, depth + 1, budget)?
        ),
        Predicate::And { predicates } | Predicate::Or { predicates } => {
            let and = matches!(predicate, Predicate::And { .. });
            let parts = predicates
                .iter()
                .map(|p| node(source, p, params, depth + 1, budget))
                .collect::<Result<Vec<_>, _>>()?;
            if parts.is_empty() {
                if and { "TRUE" } else { "FALSE" }.into()
            } else {
                format!("({})", parts.join(if and { " AND " } else { " OR " }))
            }
        }
        Predicate::Compare {
            field,
            operator,
            operand,
            case_sensitive,
            transform,
        } => {
            let Resolved {
                definition,
                column,
                json_column,
                shape,
                numeric,
            } = source.resolve(field, transform.as_deref(), params)?;
            let definition = &definition;
            let literal = match operand {
                Operand::Value { value } => Some(value),
                _ => None,
            };
            let op = operator.as_str();
            if matches!(op, "exists" | "not exists") {
                if case_sensitive.is_some() || transform.is_some() || !source.is_nested(field) {
                    return Err(invalid());
                }
                let present =
                    literal.and_then(Json::as_bool).ok_or_else(invalid)? ^ (op == "not exists");
                return Ok(format!(
                    "({} IS {}NULL)",
                    json_column.as_ref().ok_or_else(invalid)?,
                    if present { "NOT " } else { "" }
                ));
            }
            if let Some(hulladb_schema::JsonShape::Array { item }) =
                shape.as_ref().map(hulladb_schema::JsonShape::non_null)
            {
                let (negative, base) = op
                    .strip_prefix("not ")
                    .map(|v| (true, v))
                    .unwrap_or((false, op));
                if matches!(
                    base,
                    "contains" | "containsAny" | "containsAll" | "containedBy"
                ) {
                    if case_sensitive.is_some() {
                        return Err(invalid());
                    }
                    let supplied = literal.ok_or_else(invalid)?;
                    let values = if base == "contains" {
                        vec![supplied.clone()]
                    } else {
                        supplied.as_array().ok_or_else(invalid)?.clone()
                    };
                    if values.len() > 1024 {
                        return Err(invalid());
                    }
                    for value in &values {
                        item.validate(value).map_err(|_| invalid())?;
                    }
                    params.push(Box::new(Json::Array(values)));
                    let rhs = format!("${}::jsonb", params.len());
                    let raw = json_column.as_ref().ok_or_else(invalid)?;
                    let expression = match base {
                        "contains" | "containsAny" => format!(
                            "EXISTS (SELECT 1 FROM jsonb_array_elements({raw}) a(v), jsonb_array_elements({rhs}) b(v) WHERE a.v=b.v)"
                        ),
                        "containsAll" => format!(
                            "NOT EXISTS (SELECT 1 FROM jsonb_array_elements({rhs}) b(v) WHERE NOT EXISTS (SELECT 1 FROM jsonb_array_elements({raw}) a(v) WHERE a.v=b.v))"
                        ),
                        _ => format!(
                            "NOT EXISTS (SELECT 1 FROM jsonb_array_elements({raw}) a(v) WHERE NOT EXISTS (SELECT 1 FROM jsonb_array_elements({rhs}) b(v) WHERE a.v=b.v))"
                        ),
                    };
                    return Ok(format!(
                        "(CASE WHEN jsonb_typeof({raw})='array' THEN {}({expression}) ELSE NULL END)",
                        if negative { "NOT " } else { "" }
                    ));
                }
            }
            let mut bind = |value: &Json| -> Result<String, Error> {
                if params.len() >= 4096 {
                    return Err(invalid());
                }
                if let Some(kind) = numeric {
                    let text = value
                        .as_str()
                        .filter(|s| valid_numeric(s, kind))
                        .ok_or_else(invalid)?;
                    params.push(Box::new(text.to_owned()));
                    return Ok(format!("${}::text::numeric", params.len()));
                }
                let decoded = decode_value(definition, value)?;
                params.push(parameter(definition, &decoded)?);
                Ok(format!("${}", params.len()))
            };
            let literal = match operand {
                Operand::Value { value } => Some(value),
                _ => None,
            };
            if literal.is_some_and(Json::is_null) && matches!(op, "=" | "!=" | "is" | "is not") {
                if case_sensitive.is_some() {
                    return Err(invalid());
                }
                format!(
                    "({} IS {}NULL)",
                    json_column
                        .as_ref()
                        .map(|raw| format!("NULLIF({raw}, 'null'::jsonb)"))
                        .unwrap_or_else(|| column.clone()),
                    if matches!(op, "!=" | "is not") {
                        "NOT "
                    } else {
                        ""
                    }
                )
            } else if matches!(op, "in" | "not in") {
                if case_sensitive.is_some() {
                    return Err(invalid());
                }
                let values = literal.and_then(Json::as_array).ok_or_else(invalid)?;
                if values.len() > 1024 {
                    return Err(invalid());
                }
                let mut bound = Vec::new();
                let mut null = false;
                for value in values {
                    if value.is_null() {
                        null = true;
                    } else {
                        bound.push(bind(value)?);
                    }
                }
                let positive = if bound.is_empty() {
                    if null {
                        format!("{column} IS NULL")
                    } else {
                        "FALSE".into()
                    }
                } else if null {
                    format!("({column} IN ({}) OR {column} IS NULL)", bound.join(","))
                } else {
                    format!("{column} IN ({})", bound.join(","))
                };
                if op == "not in" {
                    format!("(NOT ({positive}))")
                } else {
                    format!("({positive})")
                }
            } else if matches!(op, "between" | "not between") {
                if matches!(
                    definition.scalar_type,
                    ScalarType::Boolean | ScalarType::Json
                ) || case_sensitive.is_some()
                {
                    return Err(invalid());
                }
                let values = literal
                    .and_then(Json::as_array)
                    .filter(|v| v.len() == 2 && !v.iter().any(Json::is_null))
                    .ok_or_else(invalid)?;
                format!(
                    "({column} {} {} AND {})",
                    op.to_uppercase(),
                    bind(&values[0])?,
                    bind(&values[1])?
                )
            } else if matches!(op, "=" | "!=" | ">" | ">=" | "<" | "<=") {
                if case_sensitive.is_some()
                    || (matches!(
                        definition.scalar_type,
                        ScalarType::Boolean | ScalarType::Json
                    ) && !matches!(op, "=" | "!="))
                    || literal.is_some_and(Json::is_null)
                {
                    return Err(invalid());
                }
                let rhs = match operand {
                    Operand::Value { value } => bind(value)?,
                    Operand::Field { field, transform } => {
                        let other = source.resolve(field, transform.as_deref(), params)?;
                        if other.definition.scalar_type != definition.scalar_type
                            || (numeric == Some(NumericKind::Decimal))
                                != (other.numeric == Some(NumericKind::Decimal))
                        {
                            return Err(invalid());
                        }
                        other.column
                    }
                };
                format!("({column} {op} {rhs})")
            } else {
                if definition.scalar_type != ScalarType::String {
                    return Err(invalid());
                }
                let (negative, base) = op
                    .strip_prefix("not ")
                    .map(|v| (true, v))
                    .unwrap_or((false, op));
                if !matches!(
                    base,
                    "contains" | "startsWith" | "endsWith" | "like" | "ilike"
                ) {
                    return Err(invalid());
                }
                let text = literal.and_then(Json::as_str).ok_or_else(invalid)?;
                if matches!(base, "like" | "ilike") && case_sensitive.is_some() {
                    return Err(invalid());
                }
                let escaped = text
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_");
                let pattern = match base {
                    "contains" => format!("%{escaped}%"),
                    "startsWith" => format!("{escaped}%"),
                    "endsWith" => format!("%{escaped}"),
                    _ => text.into(),
                };
                let rhs = bind(&Json::String(pattern))?;
                format!(
                    "({column} {}{} {rhs})",
                    if negative { "NOT " } else { "" },
                    if base == "ilike" || case_sensitive == &Some(false) {
                        "ILIKE"
                    } else {
                        "LIKE"
                    }
                )
            }
        }
    })
}

fn decode_value(field: &hulladb_schema::FieldDefinition, value: &Json) -> Result<Value, Error> {
    match field.scalar_type {
        ScalarType::Json => {
            field
                .json_shape
                .as_ref()
                .unwrap_or(&hulladb_schema::JsonShape::Any)
                .validate(value)
                .map_err(|_| invalid())?;
            Some(Value::Json(value.clone()))
        }
        ScalarType::Float64 => value.as_number().cloned().map(Value::Float64),
        ScalarType::Date => value.as_str().map(|v| Value::Date(v.into())),
        ScalarType::Timestamp => value.as_str().map(|v| Value::Timestamp(v.into())),
        ScalarType::LocalTimestamp => value.as_str().map(|v| Value::LocalTimestamp(v.into())),
        ScalarType::String => value.as_str().map(|v| Value::String(v.into())),
        ScalarType::Boolean => value.as_bool().map(Value::Boolean),
        ScalarType::Int64 => value
            .as_str()
            .and_then(|s| s.parse::<i64>().ok().filter(|v| v.to_string() == s))
            .map(Value::Int64),
        ScalarType::Uuid => value
            .as_str()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .map(|v| Value::Uuid(*v.as_bytes())),
    }
    .ok_or_else(invalid)
}

fn resolve(
    schema: &ManagedSchema,
    table: &TableContract,
    field: &FieldPath,
    transform: Option<&str>,
    params: &mut Vec<Box<dyn ToSql + Sync + Send>>,
) -> Result<
    (
        hulladb_schema::FieldDefinition,
        String,
        Option<String>,
        Option<hulladb_schema::JsonShape>,
    ),
    Error,
> {
    use hulladb_schema::{FieldDefinition, JsonShape};
    let segments = field.segments(table);
    let root = segments.first().ok_or_else(invalid)?.as_str();
    let path = segments[1..].to_vec();
    if path.iter().any(String::is_empty) || path.len() > 64 {
        return Err(invalid());
    }
    let source = table.field(root).ok_or_else(invalid)?;
    let mut column = schema.column(table, root)?;
    let mut definition = source.clone();
    let mut raw = None;
    let mut shape = None;
    if source.scalar_type == ScalarType::Json {
        let mut current = source.json_shape.as_ref().unwrap_or(&JsonShape::Any);
        if !path.is_empty() {
            for segment in &path {
                current = current.property(segment).ok_or_else(invalid)?;
            }
            params.push(Box::new(path));
            column = format!("({column} #> ${}::text[])", params.len());
        }
        raw = Some(column.clone());
        shape = Some(current.clone());
        definition = FieldDefinition::new(
            root,
            match current.non_null() {
                JsonShape::String => ScalarType::String,
                JsonShape::Boolean => ScalarType::Boolean,
                JsonShape::Number => ScalarType::Float64,
                _ => ScalarType::Json,
            },
        );
        definition.nullable = true;
        definition.json_shape = Some(current.clone());
        column = match definition.scalar_type {
            ScalarType::String => format!("({column} #>> '{{}}')"),
            ScalarType::Boolean => format!("({column} #>> '{{}}')::boolean"),
            ScalarType::Float64 => format!("({column} #>> '{{}}')::double precision"),
            _ => format!("NULLIF({column}, 'null'::jsonb)"),
        };
    } else if !path.is_empty() {
        return Err(invalid());
    }
    if let Some(transform) = transform {
        if transform != "arrayLength"
            || !matches!(
                shape.as_ref().map(JsonShape::non_null),
                Some(JsonShape::Array { .. })
            )
        {
            return Err(invalid());
        }
        let expression = raw.as_ref().ok_or_else(invalid)?;
        column = format!(
            "(CASE WHEN jsonb_typeof({expression})='array' THEN jsonb_array_length({expression})::double precision ELSE NULL END)"
        );
        definition = FieldDefinition::new(root, ScalarType::Float64);
        definition.nullable = true;
        raw = None;
        shape = None;
    }
    Ok((definition, column, raw, shape))
}

/// Strings retain positional dotted-path syntax; segments preserve literal property names.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum FieldPath {
    Name(String),
    Segments(Vec<String>),
}
impl From<Vec<String>> for FieldPath {
    fn from(value: Vec<String>) -> Self {
        Self::Segments(value)
    }
}
impl From<String> for FieldPath {
    fn from(value: String) -> Self {
        Self::Name(value)
    }
}
impl From<&str> for FieldPath {
    fn from(value: &str) -> Self {
        Self::Name(value.into())
    }
}
impl FieldPath {
    fn segments(&self, table: &TableContract) -> Vec<String> {
        match self {
            Self::Name(name) if table.field(name).is_some() => vec![name.clone()],
            Self::Name(name) => name.split('.').map(str::to_owned).collect(),
            Self::Segments(parts) => parts.clone(),
        }
    }
    fn is_nested(&self, table: &TableContract) -> bool {
        self.segments(table).len() > 1
    }
}

fn valid_numeric(value: &str, kind: NumericKind) -> bool {
    if value.len() > 16_384 {
        return false;
    }
    let unsigned = value.strip_prefix('-').unwrap_or(value);
    let mut parts = unsigned.split('.');
    let integer = parts.next().unwrap_or("");
    if integer.is_empty()
        || !integer.bytes().all(|c| c.is_ascii_digit())
        || (integer.len() > 1 && integer.starts_with('0'))
    {
        return false;
    }
    match parts.next() {
        None => kind == NumericKind::Decimal || value != "-0",
        Some(fraction) => {
            kind == NumericKind::Decimal
                && !fraction.is_empty()
                && fraction.bytes().all(|c| c.is_ascii_digit())
                && parts.next().is_none()
        }
    }
}
