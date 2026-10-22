//! Stable keyset discovery and fixed live ranges over scalar predicates and orders.

use hulladb_schema::{ScalarType, Value};
use tokio_postgres::types::ToSql;

use super::{
    Error, ReadDb, Row,
    db::{decode_projected_row, parameter, parameter_refs, projection_columns},
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    pub table: String,
    pub projection: Option<Vec<String>>,
    pub filters: Vec<super::Predicate>,
    pub order: Vec<super::Order>,
}
#[derive(Debug, Clone, Copy)]
pub struct PageBudget {
    pub max_rows: u16,
    /// Sum of PostgreSQL row_to_json candidate bytes, including its physical
    /// field names and storage metadata. This is NOT an SDK wire-byte bound.
    pub max_row_json_bytes: usize,
}
impl Default for PageBudget {
    fn default() -> Self {
        Self {
            max_rows: 1000,
            max_row_json_bytes: 16 * 1024 * 1024,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    namespace: String,
    scope: uuid::Uuid,
    query: PageQuery,
    schema: serde_json::Value,
    source: uuid::Uuid,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageCursor {
    binding: [u8; 32],
    after: Vec<serde_json::Value>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRange {
    binding: Binding,
    lower: Option<Vec<serde_json::Value>>,
    upper: Option<Vec<serde_json::Value>>,
}
#[derive(Debug, Clone)]
pub struct RangePage {
    pub rows: Vec<Row>,
    /// Keep this range when observing this page; refreshing does not refill a
    /// fixed count or move its boundaries when rows are deleted or re-ordered.
    pub range: PageRange,
    pub cursor: Option<PageCursor>,
    pub row_json_bytes: usize,
}

impl ReadDb<'_> {
    /// One-time page discovery. The next cursor and live range share exactly the
    /// same tuple boundary, including a deterministic primary-key tie-breaker.
    pub async fn page(
        &self,
        query: &PageQuery,
        limit: u16,
        cursor: Option<&PageCursor>,
        budget: PageBudget,
    ) -> Result<RangePage, Error> {
        validate_budget(budget)?;
        if limit == 0 || limit > budget.max_rows {
            return Err(Error::InvalidInput(
                "page limit must fit its row budget".into(),
            ));
        }
        let binding = self.page_binding(query).await?;
        if cursor.is_some_and(|cursor| cursor.binding != binding.digest()) {
            return Err(Error::InvalidInput(
                "cursor belongs to different query conditions, ordering or scope".into(),
            ));
        }
        let lower = cursor.map(|cursor| cursor.after.clone());
        let (mut rows, row_json_bytes, has_more) = self
            .range_rows(query, lower.as_deref(), None, limit, budget, true)
            .await?;
        let upper = if has_more {
            Some(order_key(
                self,
                query,
                rows.last().ok_or(Error::CorruptData)?,
            )?)
        } else {
            None
        };
        let cursor = upper.clone().map(|after| PageCursor {
            binding: binding.digest(),
            after,
        });
        retain_projection(&mut rows, query.projection.as_deref());
        Ok(RangePage {
            rows,
            range: PageRange {
                binding,
                lower,
                upper,
            },
            cursor,
            row_json_bytes,
        })
    }

    /// Refresh an established range at this query's snapshot. Overflow returns an
    /// error; callers must not install a truncated page or mark it exhausted.
    pub async fn refresh_page(
        &self,
        range: &PageRange,
        budget: PageBudget,
    ) -> Result<RangePage, Error> {
        validate_budget(budget)?;
        if range.binding != self.page_binding(&range.binding.query).await? {
            return Err(Error::InvalidInput(
                "page range belongs to another database scope or source".into(),
            ));
        }
        let (mut rows, row_json_bytes, _) = self
            .range_rows(
                &range.binding.query,
                range.lower.as_deref(),
                range.upper.as_deref(),
                budget.max_rows,
                budget,
                false,
            )
            .await?;
        let cursor = range.upper.clone().map(|after| PageCursor {
            binding: range.binding.digest(),
            after,
        });
        retain_projection(&mut rows, range.binding.query.projection.as_deref());
        Ok(RangePage {
            rows,
            range: range.clone(),
            cursor,
            row_json_bytes,
        })
    }

    async fn range_rows(
        &self,
        query: &PageQuery,
        lower: Option<&[serde_json::Value]>,
        upper: Option<&[serde_json::Value]>,
        row_limit: u16,
        budget: PageBudget,
        discovery: bool,
    ) -> Result<(Vec<Row>, usize, bool), Error> {
        let table = self.schema.table(&query.table)?;
        let order = order_fields(self, query)?;
        // Validate the public selection before adding private cursor columns.
        projection_columns(self.schema, table, query.projection.as_deref())?;
        let mut fetched = query.projection.clone();
        if let Some(fields) = &mut fetched {
            for item in &order {
                if !fields.contains(&item.field) {
                    fields.push(item.field.clone());
                }
            }
        }
        let columns = projection_columns(self.schema, table, fetched.as_deref())?;
        let mut parameters: Vec<Box<dyn ToSql + Sync + Send>> = vec![Box::new(self.scope)];
        let mut predicates = vec![super::predicate::compile(
            self.schema,
            table,
            &query.filters,
            &mut parameters,
        )?];
        for (bound, upper) in [(lower, false), (upper, true)] {
            if let Some(bound) = bound {
                if bound.len() != order.len() {
                    return Err(invalid_token());
                }
                let mut equal = Vec::new();
                let mut alternatives = Vec::new();
                for (item, value) in order.iter().zip(bound) {
                    let field = table.field(&item.field).ok_or_else(invalid_token)?;
                    let value = decode_key(field.scalar_type, value)?;
                    parameters.push(parameter(field, &value)?);
                    let column = self.schema.column(table, &item.field)?;
                    let param = format!("${}", parameters.len());
                    // PostgreSQL defaults: ASC NULLS LAST, DESC NULLS FIRST.
                    let desc = item.direction == super::Direction::Desc;
                    let after = match (&value, desc) {
                        (Value::Null, false) => "FALSE".to_owned(),
                        (Value::Null, true) => format!("{column} IS NOT NULL"),
                        (_, false) => format!("({column} > {param} OR {column} IS NULL)"),
                        (_, true) => format!("{column} < {param}"),
                    };
                    let prefix = if equal.is_empty() {
                        String::new()
                    } else {
                        format!("{} AND ", equal.join(" AND "))
                    };
                    alternatives.push(format!("({prefix}COALESCE(({after}),FALSE))"));
                    equal.push(format!("{column} IS NOT DISTINCT FROM {param}"));
                }
                let after = format!("({})", alternatives.join(" OR "));
                predicates.push(if upper { format!("NOT {after}") } else { after });
            }
        }
        let ordering = super::predicate::ordering(self.schema, table, &query.order)?;
        parameters.push(Box::new(i64::from(row_limit) + 1));
        let selection = format!(
            "SELECT {columns} FROM {} WHERE {} ORDER BY {ordering} LIMIT ${}",
            self.schema.table_relation(table),
            predicates.join(" AND "),
            parameters.len()
        );
        self.reads
            .lock()
            .map_err(|_| Error::CorruptData)?
            .insert(query.table.clone());
        let refs = parameter_refs(&parameters);
        // Count one extra without fetching its values. Discovery's lookahead is
        // not part of the delivered page's byte budget.
        let count: i64 = self
            .tx
            .query_one(
                &format!("SELECT count(*) FROM ({selection}) AS candidate"),
                &refs,
            )
            .await?
            .try_get(0)?;
        if !discovery && count > i64::from(budget.max_rows) {
            return Err(Error::QuotaExceeded);
        }
        *parameters.last_mut().ok_or(Error::CorruptData)? = Box::new(i64::from(row_limit));
        let refs = parameter_refs(&parameters);
        let bytes: i64 = self.tx.query_one(&format!("SELECT COALESCE(sum(octet_length(row_to_json(candidate)::text)),0)::bigint FROM ({selection}) AS candidate"), &refs).await?.try_get(0)?;
        let bytes = usize::try_from(bytes).map_err(|_| Error::CorruptData)?;
        if bytes > budget.max_row_json_bytes {
            return Err(Error::QuotaExceeded);
        }
        let rows = self
            .query_rows(&selection, &refs)
            .await?
            .iter()
            .map(|row| decode_projected_row(table, row, fetched.as_deref()))
            .collect::<Result<Vec<_>, Error>>()?;
        Ok((rows, bytes, count > i64::from(row_limit)))
    }
}

fn retain_projection(rows: &mut [Row], projection: Option<&[String]>) {
    if let Some(fields) = projection {
        for row in rows {
            row.values.retain(|field, _| fields.contains(field));
        }
    }
}

fn validate_budget(budget: PageBudget) -> Result<(), Error> {
    if budget.max_rows == 0
        || budget.max_rows > 1000
        || budget.max_row_json_bytes == 0
        || budget.max_row_json_bytes > 64 * 1024 * 1024
    {
        return Err(Error::InvalidInput("invalid live page budget".into()));
    }
    Ok(())
}
fn order_fields(db: &ReadDb<'_>, query: &PageQuery) -> Result<Vec<super::Order>, Error> {
    let table = db.schema.table(&query.table)?;
    super::predicate::ordering(db.schema, table, &query.order)?;
    let mut order = query.order.clone();
    let key = &table.primary_key().name;
    if !order.iter().any(|item| &item.field == key) {
        order.push(super::Order {
            field: key.clone(),
            direction: order
                .last()
                .map(|item| item.direction)
                .unwrap_or(super::Direction::Asc),
        });
    }
    Ok(order)
}
fn order_key(
    db: &ReadDb<'_>,
    query: &PageQuery,
    row: &Row,
) -> Result<Vec<serde_json::Value>, Error> {
    order_fields(db, query)?
        .iter()
        .map(|item| {
            Ok(
                match row.values.get(&item.field).ok_or(Error::CorruptData)? {
                    Value::Json(value) => value.clone(),
                    Value::Float64(value) => serde_json::json!(value),
                    Value::Null => serde_json::Value::Null,
                    Value::Date(value)
                    | Value::Timestamp(value)
                    | Value::LocalTimestamp(value)
                    | Value::String(value) => serde_json::json!(value),
                    Value::Boolean(value) => serde_json::json!(value),
                    Value::Int64(value) => serde_json::json!(value.to_string()),
                    Value::Uuid(value) => serde_json::json!(uuid::Uuid::from_bytes(*value)),
                },
            )
        })
        .collect()
}
fn decode_key(kind: ScalarType, value: &serde_json::Value) -> Result<Value, Error> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    Ok(match kind {
        ScalarType::Json => return Err(invalid_token()),
        ScalarType::Float64 => Value::Float64(value.as_number().ok_or_else(invalid_token)?.clone()),
        ScalarType::Date => Value::Date(value.as_str().ok_or_else(invalid_token)?.into()),
        ScalarType::Timestamp => Value::Timestamp(value.as_str().ok_or_else(invalid_token)?.into()),
        ScalarType::LocalTimestamp => {
            Value::LocalTimestamp(value.as_str().ok_or_else(invalid_token)?.into())
        }
        ScalarType::String => Value::String(value.as_str().ok_or_else(invalid_token)?.into()),
        ScalarType::Boolean => Value::Boolean(value.as_bool().ok_or_else(invalid_token)?),
        ScalarType::Int64 => {
            let text = value.as_str().ok_or_else(invalid_token)?;
            let parsed: i64 = text.parse().map_err(|_| invalid_token())?;
            if text != parsed.to_string() {
                return Err(invalid_token());
            }
            Value::Int64(parsed)
        }
        ScalarType::Uuid => Value::Uuid(
            *uuid::Uuid::parse_str(value.as_str().ok_or_else(invalid_token)?)
                .map_err(|_| invalid_token())?
                .as_bytes(),
        ),
    })
}
fn invalid_token() -> Error {
    Error::InvalidInput("invalid page token".into())
}
const MAX_TOKEN_BYTES: usize = 64 * 1024;
fn encode_token<T: serde::Serialize>(value: &T) -> Result<String, Error> {
    let value = serde_json::to_vec(value).map_err(|_| Error::CorruptData)?;
    if value.len() > MAX_TOKEN_BYTES / 2 {
        return Err(Error::QuotaExceeded);
    }
    Ok(value.iter().map(|byte| format!("{byte:02x}")).collect())
}
fn decode_token<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, Error> {
    if value.len() > MAX_TOKEN_BYTES || !value.len().is_multiple_of(2) {
        return Err(invalid_token());
    }
    let bytes = value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|part| {
            let digit = |byte: u8| match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                _ => None,
            };
            Ok(digit(part[0]).ok_or_else(invalid_token)? * 16
                + digit(part[1]).ok_or_else(invalid_token)?)
        })
        .collect::<Result<Vec<u8>, Error>>()?;
    serde_json::from_slice(&bytes).map_err(|_| invalid_token())
}
impl PageCursor {
    pub fn encode(&self) -> Result<String, Error> {
        encode_token(self)
    }
    pub fn decode(value: &str) -> Result<Self, Error> {
        decode_token(value)
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RangeToken {
    binding: [u8; 32],
    lower: Option<Vec<serde_json::Value>>,
    upper: Option<Vec<serde_json::Value>>,
}
impl Binding {
    fn digest(&self) -> [u8; 32] {
        super::input_digest::input_digest(&serde_json::to_value(self).expect("page binding JSON"))
    }
}
impl ReadDb<'_> {
    async fn page_binding(&self, query: &PageQuery) -> Result<Binding, Error> {
        let (source, _) = super::store::head(self.tx, self.schema, self.scope, false).await?;
        Ok(Binding {
            namespace: self.schema.namespace().into(),
            scope: self.scope,
            query: query.clone(),
            schema: self.schema.descriptor(),
            source,
        })
    }
    pub async fn decode_page_range(
        &self,
        query: &PageQuery,
        value: &str,
        cursor: Option<&PageCursor>,
    ) -> Result<PageRange, Error> {
        let token: RangeToken = decode_token(value)?;
        let binding = self.page_binding(query).await?;
        if token.binding != binding.digest()
            || cursor.is_some_and(|cursor| cursor.binding != binding.digest())
            || token.lower.as_ref() != cursor.map(|cursor| &cursor.after)
        {
            return Err(invalid_token());
        }
        Ok(PageRange {
            binding,
            lower: token.lower,
            upper: token.upper,
        })
    }
}
impl PageRange {
    pub fn encode(&self) -> Result<String, Error> {
        encode_token(&RangeToken {
            binding: self.binding.digest(),
            lower: self.lower.clone(),
            upper: self.upper.clone(),
        })
    }
}

#[cfg(test)]
#[path = "../../tests/unit/live_ranges.rs"]
mod tests;

/// Per-invocation collector; authorization runs without this channel.
#[derive(Default)]
pub(super) struct PageExecution {
    pub requested: Option<String>,
    pub produced: std::sync::Mutex<Option<hulladb_protocol::functions::PageMetadata>>,
}
impl PageExecution {
    pub fn metadata(&self) -> Result<Option<hulladb_protocol::functions::PageMetadata>, Error> {
        Ok(self
            .produced
            .lock()
            .map_err(|_| Error::CorruptData)?
            .clone())
    }
}
