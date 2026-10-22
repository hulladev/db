use std::{
    collections::BTreeSet,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use hulladb_schema::{
    FieldDefinition, InsertKeyAction, KeyGeneration, RecordValues, ScalarType, TableContract, Value,
};
use tokio_postgres::{Row as PgRow, Transaction, types::ToSql};
use uuid::Uuid;

use super::{Context, Error, ManagedSchema, MutationContext, Registry, Row};

/// A bounded predicate with deterministic primary-key ordering. Equality uses
/// schema values (including explicit null); after is an exclusive keyset bound.
#[derive(Debug, Clone)]
pub struct Select {
    pub equals: RecordValues,
    pub after: Option<Value>,
    pub limit: u16,
}

impl Default for Select {
    fn default() -> Self {
        Self {
            equals: RecordValues::new(),
            after: None,
            limit: 100,
        }
    }
}

#[derive(Clone, Copy)]
pub struct ReadDb<'a> {
    pub(super) tx: &'a Transaction<'a>,
    pub(super) schema: &'a ManagedSchema,
    pub(super) scope: Uuid,
    pub(super) reads: &'a Mutex<BTreeSet<String>>,
    pub(super) read_budget: &'a Mutex<ReadBudget>,
}

/// One invocation's fetched PostgreSQL rows, including RETURNING and aggregate
/// results. This does not estimate rows scanned internally by PostgreSQL.
#[derive(Default)]
pub(super) struct ReadBudget {
    limits: Option<(usize, usize)>,
    rows: usize,
    bytes: usize,
    exceeded: bool,
}

struct RawColumnSize(usize);
impl<'a> tokio_postgres::types::FromSql<'a> for RawColumnSize {
    fn from_sql(
        _: &tokio_postgres::types::Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(Self(raw.len()))
    }
    fn accepts(_: &tokio_postgres::types::Type) -> bool {
        true
    }
}

impl<'a> ReadDb<'a> {
    /// Configure once before executing the invocation. Fetched byte usage counts
    /// PostgreSQL binary column payloads, excluding transport framing and indexes.
    #[doc(hidden)]
    pub fn set_read_limits(&self, max_rows: usize, max_bytes: usize) -> Result<(), Error> {
        if max_rows == 0 || max_bytes == 0 {
            return Err(Error::InvalidInput("read budgets must be positive".into()));
        }
        let mut budget = self.read_budget.lock().map_err(|_| Error::CorruptData)?;
        if budget.limits == Some((max_rows, max_bytes)) {
            return self.check_read_budget_state(&budget);
        }
        if budget.rows != 0 || budget.limits.is_some() || budget.exceeded {
            return Err(Error::InvalidInput(
                "read budgets must be configured before execution".into(),
            ));
        }
        budget.limits = Some((max_rows, max_bytes));
        Ok(())
    }
    fn check_read_budget_state(&self, budget: &ReadBudget) -> Result<(), Error> {
        if budget.exceeded {
            Err(Error::TransactionDeadline)
        } else {
            Ok(())
        }
    }
    #[doc(hidden)]
    pub fn read_usage(&self) -> Result<(usize, usize), Error> {
        let budget = self.read_budget.lock().map_err(|_| Error::CorruptData)?;
        if budget.exceeded {
            return Err(Error::TransactionDeadline);
        }
        Ok((budget.rows, budget.bytes))
    }
    /// Poll the driver's bounded stream instead of allocating an unbounded Vec
    /// with `Transaction::query`. A breached budget poisons this invocation even
    /// if application code catches the individual operation error.
    pub(super) async fn query_rows(
        &self,
        sql: &str,
        parameters: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<PgRow>, Error> {
        self.read_usage()?;
        let stream = self.tx.query_raw(sql, parameters.iter().copied()).await?;
        let mut stream = std::pin::pin!(stream);
        let mut rows = Vec::new();
        while let Some(row) =
            std::future::poll_fn(|cx| futures_core::Stream::poll_next(stream.as_mut(), cx)).await
        {
            let row = row?;
            let mut bytes = 0usize;
            for index in 0..row.len() {
                bytes = bytes.saturating_add(
                    row.try_get::<_, Option<RawColumnSize>>(index)?
                        .map_or(0, |value| value.0),
                );
            }
            {
                let mut budget = self.read_budget.lock().map_err(|_| Error::CorruptData)?;
                budget.rows = budget.rows.saturating_add(1);
                budget.bytes = budget.bytes.saturating_add(bytes);
                if let Some((max_rows, max_bytes)) = budget.limits {
                    budget.exceeded = budget.rows > max_rows || budget.bytes > max_bytes;
                }
                if budget.exceeded {
                    return Err(Error::TransactionDeadline);
                }
            }
            rows.push(row);
        }
        Ok(rows)
    }

    /// Configure trusted direct-scope SQL deadlines alongside its host timer.
    pub async fn transaction_limits(self, execution_ms: u64) -> Result<(), Error> {
        if execution_ms == 0 || execution_ms > 60_000 {
            return Err(Error::InvalidInput("invalid execution deadline".into()));
        }
        self.tx
            .batch_execute(&format!(
                "SET LOCAL statement_timeout = '{}ms'; SET LOCAL lock_timeout = '{}ms'",
                execution_ms, execution_ms
            ))
            .await?;
        Ok(())
    }

    /// Canonical metadata for typed authoring adapters. This exposes no driver
    /// or transaction access and does not broaden the operation's data scope.
    pub fn schema(&self) -> &'a hulladb_schema::SchemaContract {
        self.schema.contract()
    }

    /// Parameterized scalar predicates for the generated statement builder.
    /// Every entry is ANDed, including repeated fields. An absent limit is not
    /// silently replaced by the legacy bounded-select default.
    pub async fn select_where(
        &self,
        table: &str,
        filters: &[super::Predicate],
        limit: Option<i64>,
        order: &[super::Order],
    ) -> Result<Vec<Row>, Error> {
        self.select_where_projected(table, filters, limit, order, None)
            .await
    }

    /// Execute with an optional explicit selection of returned columns.
    pub async fn select_where_projected(
        &self,
        table: &str,
        filters: &[super::Predicate],
        limit: Option<i64>,
        order: &[super::Order],
        projection: Option<&[String]>,
    ) -> Result<Vec<Row>, Error> {
        if limit.is_some_and(|value| !(1..=9_007_199_254_740_991).contains(&value)) {
            return Err(Error::InvalidInput(
                "query limit must be a positive safe integer".into(),
            ));
        }
        let contract = self.schema.table(table)?;
        let columns = projection_columns(self.schema, contract, projection)?;
        let mut parameters: Vec<Box<dyn ToSql + Sync + Send>> = vec![Box::new(self.scope)];
        let predicates =
            super::predicate::compile(self.schema, contract, filters, &mut parameters)?;
        let mut sql = format!(
            "SELECT {columns} FROM {} WHERE {} ORDER BY {}",
            self.schema.table_relation(contract),
            predicates,
            super::predicate::ordering(self.schema, contract, order)?,
        );
        if let Some(limit) = limit {
            parameters.push(Box::new(limit));
            sql.push_str(&format!(" LIMIT ${}", parameters.len()));
        }
        self.reads
            .lock()
            .map_err(|_| Error::CorruptData)?
            .insert(table.to_owned());
        self.query_rows(&sql, &parameter_refs(&parameters))
            .await?
            .iter()
            .map(|row| decode_projected_row(contract, row, projection))
            .collect()
    }

    pub async fn get(&self, table: &str, key: Value) -> Result<Option<Row>, Error> {
        let contract = self.schema.table(table)?;
        let filter = Select {
            equals: RecordValues::from([(contract.primary_key().name.clone(), key)]),
            limit: 1,
            ..Select::default()
        };
        Ok(self.select(table, filter).await?.pop())
    }

    pub async fn select(&self, table: &str, select: Select) -> Result<Vec<Row>, Error> {
        if select.limit == 0 || select.limit > 1000 {
            return Err(Error::InvalidInput("query limit must be 1..1000".into()));
        }
        let contract = self.schema.table(table)?;
        let mut parameters: Vec<Box<dyn ToSql + Sync + Send>> = vec![Box::new(self.scope)];
        let mut predicates = vec!["_scope=$1".to_owned()];
        for (name, value) in &select.equals {
            let field = contract
                .field(name)
                .ok_or_else(|| Error::InvalidInput(format!("unknown field {name:?}")))?;
            let column = self.schema.column(contract, name)?;
            let parameter = parameter(field, value)?;
            parameters.push(parameter);
            predicates.push(format!(
                "{column} IS NOT DISTINCT FROM ${}",
                parameters.len()
            ));
        }
        let key = contract.primary_key();
        let key_column = self.schema.column(contract, &key.name)?;
        if let Some(after) = &select.after {
            parameters.push(parameter(key, after)?);
            predicates.push(format!("{key_column} > ${}", parameters.len()));
        }
        parameters.push(Box::new(i64::from(select.limit)));
        self.reads
            .lock()
            .map_err(|_| Error::CorruptData)?
            .insert(table.to_owned());
        let sql = format!(
            "SELECT * FROM {} WHERE {} ORDER BY {key_column} LIMIT ${}",
            self.schema.table_relation(contract),
            predicates.join(" AND "),
            parameters.len()
        );
        let refs = parameter_refs(&parameters);
        self.query_rows(&sql, &refs)
            .await?
            .iter()
            .map(|row| decode_row(contract, row))
            .collect()
    }
}

/// One complete primary or unique key determines the conflict policy.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InsertConflict {
    pub fields: Vec<String>,
    pub action: InsertConflictAction,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum InsertConflictAction {
    Skip,
    Update { values: super::WriteValues },
}

pub struct WriteDb<'a, I> {
    pub(super) read: ReadDb<'a>,
    pub(super) context: &'a Context<I>,
    pub(super) registry: &'a Registry<I>,
    pub(super) dirty: &'a AtomicBool,
    pub(super) outbox: &'a super::outbox::EnqueueContext<'a>,
}

impl<'a, I: Send + Sync + 'static> WriteDb<'a, I> {
    pub fn read(&self) -> ReadDb<'a> {
        self.read
    }

    pub async fn get(&self, table: &str, key: Value) -> Result<Option<Row>, Error> {
        self.read.get(table, key).await
    }

    pub async fn select(&self, table: &str, select: Select) -> Result<Vec<Row>, Error> {
        self.read.select(table, select).await
    }

    pub async fn insert(&self, table: &str, input: RecordValues) -> Result<Row, Error> {
        self.insert_projected(table, input, None).await
    }

    /// Insert a complete validated row while returning only selected columns.
    pub async fn insert_projected(
        &self,
        table: &str,
        input: RecordValues,
        projection: Option<&[String]>,
    ) -> Result<Row, Error> {
        self.insert_many_projected(table, vec![input], projection, None)
            .await?
            .pop()
            .ok_or(Error::CorruptData)
    }

    /// One PostgreSQL statement handles the whole batch, including conflicts.
    /// Skips contribute no returned rows; duplicate updates retain PostgreSQL's
    /// cardinality error rather than updating the same row repeatedly.
    pub async fn insert_many_projected(
        &self,
        table: &str,
        inputs: Vec<RecordValues>,
        projection: Option<&[String]>,
        conflict: Option<&InsertConflict>,
    ) -> Result<Vec<Row>, Error> {
        let contract = self.read.schema.table(table)?;
        let returning = projection_columns(self.read.schema, contract, projection)?;
        let mut parameters: Vec<Box<dyn ToSql + Sync + Send>> = Vec::new();
        let mut tuples = Vec::with_capacity(inputs.len());
        for input in inputs {
            let prepared = contract.prepare_insert(input)?;
            let generate = prepared.key_action() == InsertKeyAction::Generate;
            let mut values = prepared.into_values();
            if generate {
                let key = match contract.key_generation() {
                    KeyGeneration::UuidV4 => Value::Uuid(*Uuid::new_v4().as_bytes()),
                    KeyGeneration::UuidV7 => Value::Uuid(*Uuid::now_v7().as_bytes()),
                    KeyGeneration::Custom { handler, .. } => {
                        let generator = self
                            .registry
                            .generators
                            .get(handler)
                            .ok_or_else(|| Error::MissingGenerator(handler.clone()))?;
                        generator.generate(self.context(), table).await?
                    }
                    KeyGeneration::None => return Err(Error::CorruptData),
                };
                values.insert(contract.primary_key().name.clone(), key);
            }
            contract.validate_row(&values)?;
            let mut placeholders = Vec::with_capacity(contract.fields().len() + 2);
            parameters.push(Box::new(self.context.scope));
            placeholders.push(format!("${}", parameters.len()));
            for field in contract.fields() {
                parameters.push(parameter(field, &values[&field.name])?);
                placeholders.push(format!("${}", parameters.len()));
            }
            placeholders.push("1".into());
            tuples.push(format!("({})", placeholders.join(",")));
        }
        let mut columns = vec!["_scope".to_owned()];
        columns.extend((0..contract.fields().len()).map(|index| format!("f{index}")));
        columns.push("_version".into());
        let conflict_sql = if let Some(conflict) = conflict {
            let fields: BTreeSet<_> = conflict.fields.iter().collect();
            let primary =
                conflict.fields.len() == 1 && conflict.fields[0] == contract.primary_key().name;
            let unique = contract
                .unique_constraints()
                .iter()
                .any(|key| key.fields().iter().collect::<BTreeSet<_>>() == fields);
            if fields.is_empty() || fields.len() != conflict.fields.len() || !(primary || unique) {
                return Err(Error::InvalidInput(
                    "conflict target must be a complete primary or unique key".into(),
                ));
            }
            let mut target = vec!["_scope".to_owned()];
            for field in &conflict.fields {
                target.push(self.read.schema.column(contract, field)?);
            }
            let action = match &conflict.action {
                InsertConflictAction::Skip => "DO NOTHING".to_owned(),
                InsertConflictAction::Update { values } => {
                    let mut assignments = super::write_expression::assignments(
                        self.read.schema,
                        contract,
                        values,
                        true,
                        &mut parameters,
                    )?;
                    assignments.push("_version=current._version+1".into());
                    format!("DO UPDATE SET {}", assignments.join(","))
                }
            };
            format!(" ON CONFLICT ({}) {action}", target.join(","))
        } else {
            String::new()
        };
        if tuples.is_empty() {
            return Ok(Vec::new());
        }
        if parameters.len() > 65_535 {
            return Err(Error::InvalidInput(
                "insert has too many bound values".into(),
            ));
        }
        let sql = format!(
            "INSERT INTO {} AS current ({}) VALUES {}{conflict_sql} RETURNING {returning}",
            self.read.schema.table_relation(contract),
            columns.join(","),
            tuples.join(",")
        );
        let rows = self
            .read
            .query_rows(&sql, &parameter_refs(&parameters))
            .await?;
        if !rows.is_empty() {
            self.dirty.store(true, Ordering::Relaxed);
        }
        rows.iter()
            .map(|row| decode_projected_row(contract, row, projection))
            .collect()
    }

    pub async fn update(
        &self,
        table: &str,
        key: Value,
        expected_version: i64,
        changes: RecordValues,
    ) -> Result<Row, Error> {
        let contract = self.read.schema.table(table)?;
        contract.validate_update(&changes)?;
        if changes.is_empty() || expected_version <= 0 || expected_version == i64::MAX {
            return Err(Error::InvalidInput(
                "updates require changes and a positive incrementable version".into(),
            ));
        }
        let mut parameters: Vec<Box<dyn ToSql + Sync + Send>> = vec![
            Box::new(self.context.scope),
            parameter(contract.primary_key(), &key)?,
            Box::new(expected_version),
        ];
        let mut assignments = vec!["_version=_version+1".to_owned()];
        for (name, value) in &changes {
            let field = contract.field(name).ok_or(Error::CorruptData)?;
            parameters.push(parameter(field, value)?);
            assignments.push(format!(
                "{}=${}",
                self.read.schema.column(contract, name)?,
                parameters.len()
            ));
        }
        let sql = format!(
            "UPDATE {} SET {} WHERE _scope=$1 AND {}=$2 AND _version=$3 RETURNING *",
            self.read.schema.table_relation(contract),
            assignments.join(","),
            self.read
                .schema
                .column(contract, &contract.primary_key().name)?
        );
        let refs = parameter_refs(&parameters);
        let row = self.read.query_rows(&sql, &refs).await?.pop();
        let Some(row) = row else {
            return self.missing_or_conflict(table, key).await;
        };
        self.dirty.store(true, Ordering::Relaxed);
        decode_row(contract, &row)
    }

    pub async fn delete(
        &self,
        table: &str,
        key: Value,
        expected_version: i64,
    ) -> Result<Row, Error> {
        if expected_version <= 0 {
            return Err(Error::InvalidInput(
                "expected version must be positive".into(),
            ));
        }
        let contract = self.read.schema.table(table)?;
        let key_parameter = parameter(contract.primary_key(), &key)?;
        let sql = format!(
            "DELETE FROM {} WHERE _scope=$1 AND {}=$2 AND _version=$3 RETURNING *",
            self.read.schema.table_relation(contract),
            self.read
                .schema
                .column(contract, &contract.primary_key().name)?
        );
        let row = self
            .read
            .query_rows(
                &sql,
                &[
                    &self.context.scope,
                    key_parameter.as_ref(),
                    &expected_version,
                ],
            )
            .await?
            .pop();
        let Some(row) = row else {
            return self.missing_or_conflict(table, key).await;
        };
        self.dirty.store(true, Ordering::Relaxed);
        decode_row(contract, &row)
    }

    /// Predicate writes use the enclosing transaction's OCC boundary rather than
    /// exposing row concurrency versions as application arguments.
    pub async fn update_where(
        &self,
        table: &str,
        filters: &[super::Predicate],
        changes: RecordValues,
    ) -> Result<Vec<Row>, Error> {
        self.update_where_projected(table, filters, changes, None)
            .await
    }

    /// Execute with an optional explicit selection of returned columns.
    pub async fn update_where_projected(
        &self,
        table: &str,
        filters: &[super::Predicate],
        changes: RecordValues,
        projection: Option<&[String]>,
    ) -> Result<Vec<Row>, Error> {
        let values = changes
            .into_iter()
            .map(|(key, value)| (key, super::WriteExpression::literal(value)))
            .collect();
        self.update_expressions_projected(table, filters, values, projection)
            .await
    }

    pub async fn update_expressions_projected(
        &self,
        table: &str,
        filters: &[super::Predicate],
        values: super::WriteValues,
        projection: Option<&[String]>,
    ) -> Result<Vec<Row>, Error> {
        let contract = self.read.schema.table(table)?;
        let columns = projection_columns(self.read.schema, contract, projection)?;
        let mut parameters: Vec<Box<dyn ToSql + Sync + Send>> = vec![Box::new(self.context.scope)];
        let predicates =
            super::predicate::compile(self.read.schema, contract, filters, &mut parameters)?;
        let mut assignments = super::write_expression::assignments(
            self.read.schema,
            contract,
            &values,
            false,
            &mut parameters,
        )?;
        assignments.push("_version=current._version+1".into());
        self.read
            .reads
            .lock()
            .map_err(|_| Error::CorruptData)?
            .insert(table.to_owned());
        let sql = format!(
            "UPDATE {} AS current SET {} WHERE {} RETURNING {columns}",
            self.read.schema.table_relation(contract),
            assignments.join(","),
            predicates
        );
        let rows = self
            .read
            .query_rows(&sql, &parameter_refs(&parameters))
            .await?;
        if !rows.is_empty() {
            self.dirty.store(true, Ordering::Relaxed);
        }
        rows.iter()
            .map(|row| decode_projected_row(contract, row, projection))
            .collect()
    }

    pub async fn delete_where(
        &self,
        table: &str,
        filters: &[super::Predicate],
    ) -> Result<Vec<Row>, Error> {
        self.delete_where_projected(table, filters, None).await
    }

    /// Execute with an optional explicit selection of returned columns.
    pub async fn delete_where_projected(
        &self,
        table: &str,
        filters: &[super::Predicate],
        projection: Option<&[String]>,
    ) -> Result<Vec<Row>, Error> {
        let contract = self.read.schema.table(table)?;
        let columns = projection_columns(self.read.schema, contract, projection)?;
        let mut parameters: Vec<Box<dyn ToSql + Sync + Send>> = vec![Box::new(self.context.scope)];
        let predicates =
            super::predicate::compile(self.read.schema, contract, filters, &mut parameters)?;
        self.read
            .reads
            .lock()
            .map_err(|_| Error::CorruptData)?
            .insert(table.to_owned());
        let sql = format!(
            "DELETE FROM {} WHERE {} RETURNING {columns}",
            self.read.schema.table_relation(contract),
            predicates
        );
        let rows = self
            .read
            .query_rows(&sql, &parameter_refs(&parameters))
            .await?;
        if !rows.is_empty() {
            self.dirty.store(true, Ordering::Relaxed);
        }
        rows.iter()
            .map(|row| decode_projected_row(contract, row, projection))
            .collect()
    }

    async fn missing_or_conflict(&self, table: &str, key: Value) -> Result<Row, Error> {
        if self.get(table, key).await?.is_some() {
            Err(Error::Conflict)
        } else {
            Err(Error::NotFound)
        }
    }

    fn context(&self) -> MutationContext<'a, I> {
        MutationContext {
            db: WriteDb {
                read: self.read,
                context: self.context,
                registry: self.registry,
                dirty: self.dirty,
                outbox: self.outbox,
            },
            identity: &self.context.identity,
        }
    }
}

pub(super) fn parameter(
    field: &FieldDefinition,
    value: &Value,
) -> Result<Box<dyn ToSql + Sync + Send>, Error> {
    let invalid = || Error::InvalidInput(format!("invalid value for field {:?}", field.name));
    match (field.scalar_type, value) {
        (_, Value::Null) if !field.nullable => Err(invalid()),
        (ScalarType::Date, Value::Null) => Ok(Box::new(None::<time::Date>)),
        (ScalarType::Timestamp, Value::Null) => Ok(Box::new(None::<time::OffsetDateTime>)),
        (ScalarType::LocalTimestamp, Value::Null) => Ok(Box::new(None::<time::PrimitiveDateTime>)),
        (ScalarType::Date, Value::Date(text)) => Ok(Box::new(
            hulladb_schema::temporal::date(text).ok_or_else(invalid)?,
        )),
        (ScalarType::Timestamp, Value::Timestamp(text)) => Ok(Box::new(
            hulladb_schema::temporal::timestamp(text, field.precision.unwrap_or(3), true)
                .ok_or_else(invalid)?
                .assume_utc(),
        )),
        (ScalarType::LocalTimestamp, Value::LocalTimestamp(text)) => Ok(Box::new(
            hulladb_schema::temporal::timestamp(text, field.precision.unwrap_or(3), false)
                .ok_or_else(invalid)?,
        )),
        (ScalarType::Json, Value::Null) => Ok(Box::new(None::<serde_json::Value>)),
        (ScalarType::Float64, Value::Null) => Ok(Box::new(None::<f64>)),
        (ScalarType::Json, Value::Json(value)) => {
            field
                .json_shape
                .as_ref()
                .unwrap_or(&hulladb_schema::JsonShape::Any)
                .validate(value)
                .map_err(|_| invalid())?;
            Ok(Box::new(value.clone()))
        }
        (ScalarType::Float64, Value::Float64(value)) => {
            Ok(Box::new(value.as_f64().ok_or_else(invalid)?))
        }
        (ScalarType::String, Value::Null) => Ok(Box::new(None::<String>)),
        (ScalarType::Boolean, Value::Null) => Ok(Box::new(None::<bool>)),
        (ScalarType::Int64, Value::Null) => Ok(Box::new(None::<i64>)),
        (ScalarType::Uuid, Value::Null) => Ok(Box::new(None::<Uuid>)),
        (ScalarType::String, Value::String(value)) if !value.contains('\0') => {
            Ok(Box::new(value.clone()))
        }
        (ScalarType::Boolean, Value::Boolean(value)) => Ok(Box::new(*value)),
        (ScalarType::Int64, Value::Int64(value)) => Ok(Box::new(*value)),
        (ScalarType::Uuid, Value::Uuid(value)) => Ok(Box::new(Uuid::from_bytes(*value))),
        _ => Err(invalid()),
    }
}

pub(super) fn parameter_refs(
    parameters: &[Box<dyn ToSql + Sync + Send>],
) -> Vec<&(dyn ToSql + Sync)> {
    parameters
        .iter()
        .map(|p| p.as_ref() as &(dyn ToSql + Sync))
        .collect()
}

pub(super) fn decode_row(table: &TableContract, row: &PgRow) -> Result<Row, Error> {
    decode_projected_row(table, row, None)
}

pub(super) fn decode_projected_row(
    table: &TableContract,
    row: &PgRow,
    projection: Option<&[String]>,
) -> Result<Row, Error> {
    let mut values = RecordValues::new();
    for (i, field) in table.fields().iter().enumerate() {
        if projection.is_some_and(|fields| !fields.contains(&field.name)) {
            continue;
        }
        let column = format!("f{i}");
        let value = match field.scalar_type {
            ScalarType::Date | ScalarType::Timestamp | ScalarType::LocalTimestamp => {
                temporal_value(row, column.as_str(), field.scalar_type)?
            }
            ScalarType::Json => row
                .try_get::<_, Option<serde_json::Value>>(column.as_str())?
                .map(Value::Json),
            ScalarType::Float64 => row
                .try_get::<_, Option<f64>>(column.as_str())?
                .and_then(serde_json::Number::from_f64)
                .map(Value::Float64),
            ScalarType::String => row
                .try_get::<_, Option<String>>(column.as_str())?
                .map(Value::String),
            ScalarType::Boolean => row
                .try_get::<_, Option<bool>>(column.as_str())?
                .map(Value::Boolean),
            ScalarType::Int64 => row
                .try_get::<_, Option<i64>>(column.as_str())?
                .map(Value::Int64),
            ScalarType::Uuid => row
                .try_get::<_, Option<Uuid>>(column.as_str())?
                .map(|u| Value::Uuid(*u.as_bytes())),
        }
        .unwrap_or(Value::Null);
        field
            .validate_value(&value)
            .map_err(|_| Error::CorruptData)?;
        values.insert(field.name.clone(), value);
    }
    let version = row.try_get("_version")?;
    if version <= 0 {
        return Err(Error::CorruptData);
    }
    Ok(Row { values, version })
}

/// Validate the external selection before SQL execution, including empty results.
/// Physical column names never originate from caller input.
pub(super) fn projection_columns(
    schema: &ManagedSchema,
    table: &TableContract,
    projection: Option<&[String]>,
) -> Result<String, Error> {
    let mut columns = vec!["_version".to_owned()];
    if let Some(fields) = projection {
        if fields.is_empty() || fields.len() > table.fields().len() {
            return Err(Error::InvalidInput(
                "projection must select declared columns".into(),
            ));
        }
        let mut seen = BTreeSet::new();
        for field in fields {
            if !seen.insert(field) {
                return Err(Error::InvalidInput("duplicate projection column".into()));
            }
            columns.push(schema.column(table, field)?);
        }
    } else {
        columns.extend((0..table.fields().len()).map(|index| format!("f{index}")));
    }
    Ok(columns.join(","))
}

/// Decode binary native date/time values without server timezone or DateStyle assumptions.
pub(super) fn temporal_value(
    row: &PgRow,
    column: &str,
    kind: ScalarType,
) -> Result<Option<Value>, Error> {
    use hulladb_schema::temporal::format_timestamp;
    Ok(match kind {
        ScalarType::Date => row
            .try_get::<_, Option<time::Date>>(column)?
            .map(|date| {
                if !(1..=9999).contains(&date.year()) {
                    return Err(Error::CorruptData);
                }
                Ok(Value::Date(date.to_string()))
            })
            .transpose()?,
        ScalarType::Timestamp => row
            .try_get::<_, Option<time::OffsetDateTime>>(column)?
            .map(|date| {
                let date = date.to_offset(time::UtcOffset::UTC);
                format_timestamp(time::PrimitiveDateTime::new(date.date(), date.time()), true)
                    .map(Value::Timestamp)
                    .ok_or(Error::CorruptData)
            })
            .transpose()?,
        ScalarType::LocalTimestamp => row
            .try_get::<_, Option<time::PrimitiveDateTime>>(column)?
            .map(|date| {
                format_timestamp(date, false)
                    .map(Value::LocalTimestamp)
                    .ok_or(Error::CorruptData)
            })
            .transpose()?,
        _ => return Err(Error::CorruptData),
    })
}
