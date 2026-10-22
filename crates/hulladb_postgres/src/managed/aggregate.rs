//! Aggregate in the caller's database snapshot, preserving source-table dependencies.
use super::predicate::{NumericKind, OutputColumn};
use super::{Direction, Error, Order, Predicate, ReadDb};
use hulladb_schema::Value;
use hulladb_schema::{FieldDefinition, ScalarType};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};
use std::collections::{BTreeMap, BTreeSet};
use tokio_postgres::{Row, types::ToSql};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Aggregate {
    pub op: AggregateOp,
    pub field: Option<String>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AggregateOp {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AggregateQuery {
    pub table: String,
    pub filters: Vec<Predicate>,
    pub group_by: Vec<String>,
    pub aggregates: BTreeMap<String, Aggregate>,
    pub having: Vec<Predicate>,
    pub order: Vec<Order>,
    pub limit: Option<i64>,
}
#[derive(Clone, Copy)]
enum ResultType {
    Count,
    Scalar(ScalarType),
    Numeric(NumericKind),
}
struct Column {
    name: String,
    physical: String,
    result_type: ResultType,
}
fn invalid(message: &str) -> Error {
    Error::InvalidInput(message.into())
}
impl ReadDb<'_> {
    pub async fn aggregate(&self, query: &AggregateQuery) -> Result<Json, Error> {
        if query.aggregates.is_empty()
            || query.aggregates.len() > 256
            || query.group_by.len() > 64
            || query.order.len() > 64
        {
            return Err(invalid(
                "aggregation requires 1..256 results and at most 64 grouping/ordering fields",
            ));
        }
        if query.group_by.is_empty()
            && (!query.having.is_empty() || !query.order.is_empty() || query.limit.is_some())
        {
            return Err(invalid(
                "having, aggregate ordering and limits require grouping",
            ));
        }
        if query
            .limit
            .is_some_and(|v| !(1..=9_007_199_254_740_991).contains(&v))
        {
            return Err(invalid("aggregate limit must be a positive safe integer"));
        }
        let table = self.schema.table(&query.table)?;
        let mut params: Vec<Box<dyn ToSql + Sync + Send>> = vec![Box::new(self.scope)];
        let predicates =
            super::predicate::compile(self.schema, table, &query.filters, &mut params)?;
        let mut columns = Vec::new();
        let mut selections = Vec::new();
        let mut groups = Vec::new();
        let mut names = BTreeSet::new();
        for name in &query.group_by {
            if !names.insert(name) {
                return Err(invalid("duplicate group field"));
            }
            let field = table
                .field(name)
                .ok_or_else(|| invalid("unknown group field"))?;
            if field.scalar_type == ScalarType::Json {
                return Err(invalid("JSON grouping is not supported"));
            }
            let expression = self.schema.column(table, name)?;
            let physical = format!("a{}", columns.len());
            selections.push(format!("{expression} AS {physical}"));
            groups.push(expression);
            columns.push(Column {
                name: name.clone(),
                physical,
                result_type: ResultType::Scalar(field.scalar_type),
            });
        }
        for (name, aggregate) in &query.aggregates {
            if name.is_empty() || name.len() > 1024 || !names.insert(name) {
                return Err(invalid(
                    "aggregate aliases must be nonempty and distinct from grouping fields",
                ));
            }
            let (expression, result_type) = match aggregate.op {
                AggregateOp::Count => {
                    if aggregate.field.is_some() {
                        return Err(invalid("count() does not accept a field"));
                    }
                    ("count(*)".into(), ResultType::Count)
                }
                op => {
                    let name = aggregate
                        .field
                        .as_ref()
                        .ok_or_else(|| invalid("aggregate requires a field"))?;
                    let field = table
                        .field(name)
                        .ok_or_else(|| invalid("unknown aggregate field"))?;
                    let source = self.schema.column(table, name)?;
                    let (function, result_type) = match (op, field.scalar_type) {
                        (AggregateOp::Sum, ScalarType::Int64) => {
                            ("sum", ResultType::Numeric(NumericKind::Integer))
                        }
                        (AggregateOp::Avg, ScalarType::Int64) => {
                            ("avg", ResultType::Numeric(NumericKind::Decimal))
                        }
                        (AggregateOp::Sum, ScalarType::Float64) => {
                            ("sum", ResultType::Scalar(ScalarType::Float64))
                        }
                        (AggregateOp::Avg, ScalarType::Float64) => {
                            ("avg", ResultType::Scalar(ScalarType::Float64))
                        }
                        (
                            AggregateOp::Min,
                            kind @ (ScalarType::String
                            | ScalarType::Int64
                            | ScalarType::Float64
                            | ScalarType::Date
                            | ScalarType::Timestamp
                            | ScalarType::LocalTimestamp),
                        ) => ("min", ResultType::Scalar(kind)),
                        (
                            AggregateOp::Max,
                            kind @ (ScalarType::String
                            | ScalarType::Int64
                            | ScalarType::Float64
                            | ScalarType::Date
                            | ScalarType::Timestamp
                            | ScalarType::LocalTimestamp),
                        ) => ("max", ResultType::Scalar(kind)),
                        _ => return Err(invalid("aggregate is not supported for this field type")),
                    };
                    (format!("{function}({source})"), result_type)
                }
            };
            let physical = format!("a{}", columns.len());
            selections.push(format!("{expression} AS {physical}"));
            columns.push(Column {
                name: name.clone(),
                physical,
                result_type,
            });
        }
        let output_columns = columns
            .iter()
            .map(|column| {
                let (kind, numeric, expression) = match column.result_type {
                    ResultType::Count => (
                        ScalarType::Float64,
                        None,
                        format!("{}::double precision", column.physical),
                    ),
                    ResultType::Scalar(kind) => (kind, None, column.physical.clone()),
                    ResultType::Numeric(kind) => (
                        if kind == NumericKind::Integer {
                            ScalarType::Int64
                        } else {
                            ScalarType::Float64
                        },
                        Some(kind),
                        column.physical.clone(),
                    ),
                };
                let mut definition = FieldDefinition::new(&column.name, kind);
                definition.nullable = !matches!(column.result_type, ResultType::Count);
                if matches!(kind, ScalarType::Timestamp | ScalarType::LocalTimestamp) {
                    let source_name = query
                        .aggregates
                        .get(&column.name)
                        .and_then(|aggregate| aggregate.field.as_deref())
                        .unwrap_or(&column.name);
                    definition.precision =
                        table.field(source_name).and_then(|field| field.precision);
                }
                (
                    column.name.clone(),
                    OutputColumn {
                        definition,
                        expression,
                        numeric,
                    },
                )
            })
            .collect();
        let having =
            super::predicate::compile_outputs(&output_columns, &query.having, &mut params)?;
        let returns = columns
            .iter()
            .map(|column| match column.result_type {
                ResultType::Numeric(_) => {
                    format!("{}::text AS {}", column.physical, column.physical)
                }
                _ => column.physical.clone(),
            })
            .collect::<Vec<_>>()
            .join(",");
        let mut sql = format!(
            "SELECT {returns} FROM (SELECT {} FROM {} WHERE {predicates}{} ) grouped WHERE {having}",
            selections.join(","),
            self.schema.table_relation(table),
            if groups.is_empty() {
                String::new()
            } else {
                format!(" GROUP BY {}", groups.join(","))
            }
        );
        if !groups.is_empty() {
            let mut orders = Vec::new();
            let mut ordered = BTreeSet::new();
            for order in &query.order {
                if !ordered.insert(&order.field) {
                    return Err(invalid("duplicate aggregate ordering field"));
                }
                let column = columns
                    .iter()
                    .find(|c| c.name == order.field)
                    .ok_or_else(|| invalid("unknown aggregate ordering field"))?;
                // Qualify the input alias: numeric result serialization casts to text
                // in SELECT, but sorting must still use the numeric source value.
                orders.push(format!(
                    "grouped.{} {}",
                    column.physical,
                    direction(order.direction)
                ));
            }
            let tie_direction = query
                .order
                .last()
                .map(|v| v.direction)
                .unwrap_or(Direction::Asc);
            for name in &query.group_by {
                if ordered.insert(name) {
                    let column = columns
                        .iter()
                        .find(|c| &c.name == name)
                        .ok_or(Error::CorruptData)?;
                    orders.push(format!(
                        "grouped.{} {}",
                        column.physical,
                        direction(tie_direction)
                    ));
                }
            }
            sql.push_str(&format!(" ORDER BY {}", orders.join(",")));
        }
        if let Some(limit) = query.limit {
            params.push(Box::new(limit));
            sql.push_str(&format!(" LIMIT ${}", params.len()));
        }
        self.reads
            .lock()
            .map_err(|_| Error::CorruptData)?
            .insert(query.table.clone());
        let rows = self
            .query_rows(&sql, &super::db::parameter_refs(&params))
            .await?;
        let results = rows
            .iter()
            .map(|row| decode(row, &columns))
            .collect::<Result<Vec<_>, _>>()?;
        if groups.is_empty() {
            results.into_iter().next().ok_or(Error::CorruptData)
        } else {
            Ok(Json::Array(results))
        }
    }
}
fn direction(value: Direction) -> &'static str {
    match value {
        Direction::Asc => "ASC",
        Direction::Desc => "DESC",
    }
}
fn decode(row: &Row, columns: &[Column]) -> Result<Json, Error> {
    let mut output = Map::new();
    for column in columns {
        let name = column.physical.as_str();
        let value = match column.result_type {
            ResultType::Count => {
                let count: i64 = row.try_get(name)?;
                if !(0..=9_007_199_254_740_991).contains(&count) {
                    return Err(invalid(
                        "aggregate count exceeds the JavaScript safe integer range",
                    ));
                }
                Json::from(count)
            }
            ResultType::Numeric(_) | ResultType::Scalar(ScalarType::String) => row
                .try_get::<_, Option<String>>(name)?
                .map(Json::String)
                .unwrap_or(Json::Null),
            ResultType::Scalar(ScalarType::Int64) => row
                .try_get::<_, Option<i64>>(name)?
                .map(|v| Json::String(v.to_string()))
                .unwrap_or(Json::Null),
            ResultType::Scalar(ScalarType::Float64) => match row.try_get::<_, Option<f64>>(name)? {
                None => Json::Null,
                Some(value) => Json::Number(
                    serde_json::Number::from_f64(value)
                        .ok_or_else(|| invalid("non-finite aggregate number"))?,
                ),
            },
            ResultType::Scalar(ScalarType::Boolean) => row
                .try_get::<_, Option<bool>>(name)?
                .map(Json::Bool)
                .unwrap_or(Json::Null),
            ResultType::Scalar(ScalarType::Uuid) => row
                .try_get::<_, Option<uuid::Uuid>>(name)?
                .map(|v| Json::String(v.to_string()))
                .unwrap_or(Json::Null),
            ResultType::Scalar(
                kind @ (ScalarType::Date | ScalarType::Timestamp | ScalarType::LocalTimestamp),
            ) => match super::db::temporal_value(row, name, kind)? {
                Some(Value::Date(text) | Value::Timestamp(text) | Value::LocalTimestamp(text)) => {
                    Json::String(text)
                }
                None => Json::Null,
                _ => return Err(Error::CorruptData),
            },
            ResultType::Scalar(ScalarType::Json) => return Err(Error::CorruptData),
        };
        output.insert(column.name.clone(), value);
    }
    Ok(Json::Object(output))
}
