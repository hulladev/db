//! TypeScript callbacks over scoped managed CRUD. Each invocation owns
//! one bounded worker process; PostgreSQL transactions and receipts stay native.

mod contract;
mod limits;
pub use limits::InvocationLimits;
mod schema;
pub use schema::schema_from_descriptor;

use std::{
    collections::BTreeMap, ffi::OsString, path::PathBuf, process::Stdio, sync::Arc, time::Duration,
};

use hulladb_schema::{FieldDefinition, RecordValues, ScalarType, Value};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, Command},
};
use uuid::Uuid;

use super::{
    Authorization, Error, FunctionContract, FunctionFuture, Mutation, MutationConcurrency,
    MutationContext, Query, QueryContext, ReadDb, Registry, Row, WriteDb,
};
use contract::{Shape, int64};

#[derive(Debug, Clone)]
pub struct Worker {
    pub executable: PathBuf,
    /// Runtime arguments, normally a restricted JavaScript application bundle.
    /// Configuring another executable explicitly delegates the worker capability
    /// to trusted host code; application declarations cannot select their runtime.
    pub arguments: Vec<OsString>,
    pub timeout: Duration,
    pub max_frame_bytes: usize,
    pub max_operations: usize,
    pub development: bool,
}

impl Worker {
    pub fn new(executable: impl Into<PathBuf>, arguments: Vec<OsString>) -> Self {
        Self {
            executable: executable.into(),
            arguments,
            timeout: Duration::from_secs(5),
            max_frame_bytes: InvocationLimits::default().frame_bytes(),
            max_operations: 1000,
            development: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Query,
    Mutation,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Contract {
    kind: Kind,
    input: Shape,
    result: Shape,
    #[serde(default)]
    paginated: bool,
    #[serde(default)]
    limits: Json,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    #[serde(rename = "generatedBy")]
    generated_by: String,
    functions: BTreeMap<String, Contract>,
    #[serde(default)]
    limits: Option<InvocationLimits>,
}

#[derive(Clone)]
pub struct TypescriptFunction {
    worker: Arc<Worker>,
    name: String,
    contract: Contract,
    limits: InvocationLimits,
}

impl TypescriptFunction {
    /// Load the exact input/result shapes emitted alongside generated SDK calls.
    pub fn from_manifest(worker: Arc<Worker>, name: &str, manifest: &str) -> Result<Self, Error> {
        if worker.timeout.is_zero()
            || worker.max_frame_bytes == 0
            || worker.max_frame_bytes == usize::MAX
            || worker.max_operations == 0
        {
            return Err(Error::InvalidInput(
                "worker budgets must be positive and bounded".into(),
            ));
        }
        let manifest: Manifest = serde_json::from_str(manifest)
            .map_err(|_| Error::InvalidInput("invalid generated function manifest".into()))?;
        if manifest.generated_by != "@hulla/db" {
            return Err(Error::InvalidInput("invalid manifest owner".into()));
        }
        let contract = manifest
            .functions
            .get(name)
            .cloned()
            .ok_or_else(|| Error::UnknownFunction(name.into()))?;
        if contract.paginated && contract.kind != Kind::Query {
            return Err(Error::InvalidInput(
                "only queries may declare pagination".into(),
            ));
        }
        let mut limits = manifest.limits.unwrap_or_default();
        if manifest.limits.is_none() {
            limits.execution_ms = worker
                .timeout
                .as_millis()
                .try_into()
                .map_err(|_| Error::InvalidInput("invalid worker timeout".into()))?;
            limits.operations = worker.max_operations;
        }
        limits = limits.validate()?;
        if !contract.limits.is_null() {
            limits = limits.merge(&contract.limits)?;
        }
        // Explicit embedding-host caps remain authoritative; defaults permit trusted manifests to raise budgets.
        if worker.timeout != Duration::from_secs(5) {
            limits.execution_ms = limits.execution_ms.min(worker.timeout.as_millis() as u64);
        }
        if worker.max_operations != 1000 {
            limits.operations = limits.operations.min(worker.max_operations);
        }
        Ok(Self {
            limits,
            worker,
            name: name.into(),
            contract,
        })
    }

    pub fn limits(&self) -> InvocationLimits {
        self.limits
    }

    /// Application policy runs natively in the same snapshot/transaction. Recovery
    /// returns the committed response without rerunning policy or the TypeScript handler.
    pub fn register<I: Serialize + Send + Sync + 'static>(
        self,
        registry: &mut Registry<I>,
        policy: impl Authorization<I>,
    ) -> Result<(), Error> {
        self.register_with_concurrency(registry, policy, MutationConcurrency::default())
    }

    /// More than one serializable attempt declares the whole callback and policy
    /// safe to replay after a confirmed rollback. External effects are prohibited.
    pub fn register_with_concurrency<I: Serialize + Send + Sync + 'static>(
        self,
        registry: &mut Registry<I>,
        policy: impl Authorization<I>,
        concurrency: MutationConcurrency,
    ) -> Result<(), Error> {
        match self.contract.kind {
            Kind::Query => registry.query(self.name.clone(), self, policy),
            Kind::Mutation => {
                registry.mutation_with_concurrency(self.name.clone(), self, policy, concurrency)
            }
        }
    }

    async fn execute<I: Serialize + Send + Sync + 'static>(
        &self,
        db: ReadDb<'_>,
        write: Option<&WriteDb<'_, I>>,
        identity: &I,
        input: &Json,
        page_execution: Option<&super::pagination::PageExecution>,
    ) -> Result<Json, Error> {
        let limits = self.limits;
        let frame_bytes = limits.frame_bytes().min(self.worker.max_frame_bytes);
        let started = std::time::Instant::now();
        tokio::time::timeout(Duration::from_millis(limits.execution_ms), async {
            db.tx.batch_execute(&format!("SET LOCAL statement_timeout = '{}ms'; SET LOCAL lock_timeout = '{}ms'",limits.execution_ms,limits.execution_ms)).await?;
            let mut child = Command::new(&self.worker.executable)
                .args(&self.worker.arguments)
                // No inherited database URLs, bearer credentials or deployment secrets.
                .env_clear().stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit())
                .kill_on_drop(true).spawn().map_err(|_| Error::FunctionExecution)?;
            let mut stdin = child.stdin.take().ok_or(Error::FunctionExecution)?;
            let mut stdout = BufReader::new(child.stdout.take().ok_or(Error::FunctionExecution)?);
            send(&mut stdin, &json!({"name": self.name, "kind": self.contract.kind,
                "identity": identity, "input": input, "schema": db.schema.descriptor(), "input_shape": self.contract.input, "result_shape": self.contract.result, "limits":limits}), frame_bytes).await?;
            let mut operations = 0;
            let mut native_page = None;
            loop {
                let mut line = Vec::new();
                (&mut stdout).take((frame_bytes + 1) as u64)
                    .read_until(b'\n', &mut line).await.map_err(|_| Error::FunctionExecution)?;
                if line.last() != Some(&b'\n') || line.len() > frame_bytes { return Err(Error::FunctionExecution); }
                let frame: Frame = serde_json::from_slice(&line).map_err(|_| Error::FunctionExecution)?;
                match frame {
                    Frame::Operation { id, operation } => {
                        operations += 1;
                        if operations > limits.operations { return Err(Error::TransactionDeadline); }
                        if id != operations as u64 { return Err(Error::FunctionExecution); }
                        let result = if let Operation::Paginate {table,filters,order,limit,cursor,projection} = operation {
                            if native_page.is_some() && self.contract.paginated { return Err(Error::InvalidInput("paginated query must execute exactly one paginate".into())); }
                            let requested = if self.contract.paginated {page_execution.and_then(|execution|execution.requested.as_deref())} else {None};
                            let response = paginate(db, super::PageQuery {table,filters,order,projection}, limit, cursor.as_deref(), requested).await?;
                            let range = response["range"].as_str().ok_or(Error::CorruptData)?.to_owned();
                            let cursor = response["cursor"].clone();
                            let rows = response["page"].as_array().ok_or(Error::CorruptData)?;
                            native_page = Some(json!({"page":rows.iter().map(|row|row["row"].clone()).collect::<Vec<_>>(),"cursor":cursor}));
                            if self.contract.paginated && let Some(execution) = page_execution {
                                *execution.produced.lock().map_err(|_|Error::CorruptData)? = Some(hulladb_protocol::functions::PageMetadata {range:range.clone()});
                            }
                            json!({"page":rows,"cursor":cursor,"range":range})
                        } else { operation.execute(db, write).await? };
                        send(&mut stdin, &json!({"id": id, "result": result}), frame_bytes).await?;
                    }
                    Frame::Result { result } => {
                        self.validate_result(&result)?;
                        if self.worker.development {
                            let (read_rows,read_bytes) = db.read_usage()?;
                            for (name, used, maximum) in [("executionMs",started.elapsed().as_millis() as usize,limits.execution_ms as usize),("operations",operations,limits.operations),("readRows",read_rows,limits.read_rows),("readBytes",read_bytes,limits.read_bytes),("inputBytes",serde_json::to_vec(input).map_err(|_|Error::FunctionExecution)?.len(),limits.input_bytes),("outputBytes",serde_json::to_vec(&result).map_err(|_|Error::FunctionExecution)?.len(),limits.output_bytes)] {
                                if used.saturating_mul(5) >= maximum.saturating_mul(4) { eprintln!("[hulla/db] {}: {name} {used}/{maximum}",self.name); }
                            }
                        }
                        if self.contract.paginated && native_page.as_ref() != Some(&result) {
                            return Err(Error::InvalidInput("paginated query must return its direct page result".into()));
                        }
                        drop(stdin);
                        if !child.wait().await.map_err(|_| Error::FunctionExecution)?.success() { return Err(Error::FunctionExecution); }
                        // Trailing protocol output is never accepted as a successful invocation.
                        let mut trailing = [0];
                        if stdout.read(&mut trailing).await.map_err(|_| Error::FunctionExecution)? != 0 { return Err(Error::FunctionExecution); }
                        return Ok(result);
                    }
                    Frame::ApplicationError { error } => {
                        if !error.valid() { return Err(Error::FunctionExecution); }
                        return Err(Error::Application(error));
                    }
                    Frame::Error { code } => return Err(match code.as_str() {
                        "invalid_request" => Error::InvalidInput("TypeScript input or result validation failed".into()),
                        "schema_mismatch" => Error::SchemaMismatch,
                        "execution_limit" => Error::TransactionDeadline,
                        "row_expectation_failed" => Error::RowExpectationFailed,
                        _ => Error::FunctionExecution,
                    }),
                }
            }
        }).await.map_err(|_| Error::TransactionDeadline)?
    }
}

impl FunctionContract for TypescriptFunction {
    fn read_limits(&self) -> Option<(usize, usize)> {
        Some((self.limits.read_rows, self.limits.read_bytes))
    }
    fn validate_input(&self, input: &Json) -> Result<(), Error> {
        if serde_json::to_vec(input)
            .map_err(|_| Error::FunctionExecution)?
            .len()
            > self.limits.input_bytes
        {
            return Err(Error::TransactionDeadline);
        }
        self.contract.input.validate(input)
    }
    fn validate_result(&self, result: &Json) -> Result<(), Error> {
        if serde_json::to_vec(result)
            .map_err(|_| Error::FunctionExecution)?
            .len()
            > self.limits.output_bytes
        {
            return Err(Error::TransactionDeadline);
        }
        self.contract
            .result
            .validate(result)
            .map_err(|_| Error::CorruptData)
    }
}
impl<I: Serialize + Send + Sync + 'static> Query<I> for TypescriptFunction {
    fn call<'a>(
        &'a self,
        context: QueryContext<'a, I>,
        input: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        Box::pin(async move {
            if self.contract.kind != Kind::Query {
                return Err(Error::WrongFunctionKind);
            }
            self.execute(context.db, None, context.identity, input, context.page)
                .await
        })
    }
}
impl<I: Serialize + Send + Sync + 'static> Mutation<I> for TypescriptFunction {
    fn call<'a>(
        &'a self,
        context: MutationContext<'a, I>,
        input: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        Box::pin(async move {
            if self.contract.kind != Kind::Mutation {
                return Err(Error::WrongFunctionKind);
            }
            self.execute(
                context.db.read(),
                Some(&context.db),
                context.identity,
                input,
                None,
            )
            .await
        })
    }
}

async fn send(stdin: &mut ChildStdin, value: &Json, limit: usize) -> Result<(), Error> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| Error::FunctionExecution)?;
    bytes.push(b'\n');
    if bytes.len() > limit {
        return Err(Error::QuotaExceeded);
    }
    stdin
        .write_all(&bytes)
        .await
        .map_err(|_| Error::FunctionExecution)
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Frame {
    Operation {
        id: u64,
        operation: Operation,
    },
    Result {
        result: Json,
    },
    Error {
        code: String,
    },
    ApplicationError {
        error: hulladb_protocol::ApplicationError,
    },
}

#[derive(Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Aggregate {
        table: String,
        filters: Vec<super::Predicate>,
        #[serde(rename = "groupBy")]
        group_by: Vec<String>,
        aggregates: std::collections::BTreeMap<String, super::Aggregate>,
        having: Vec<super::Predicate>,
        order: Vec<super::Order>,
        limit: Option<i64>,
    },
    Paginate {
        table: String,
        projection: Option<Vec<String>>,
        filters: Vec<super::Predicate>,
        order: Vec<super::Order>,
        limit: u16,
        cursor: Option<String>,
    },
    Select {
        table: String,
        projection: Option<Vec<String>>,
        filters: Vec<super::Predicate>,
        limit: Option<i64>,
        #[serde(default)]
        order: Vec<super::Order>,
    },
    Insert {
        table: String,
        projection: Option<Vec<String>>,
        values: Json,
        conflict: Option<super::InsertConflict>,
    },
    Update {
        table: String,
        projection: Option<Vec<String>>,
        filters: Vec<super::Predicate>,
        values: super::WriteValues,
    },
    Delete {
        table: String,
        projection: Option<Vec<String>>,
        filters: Vec<super::Predicate>,
    },
}

impl Operation {
    async fn execute<I: Send + Sync + 'static>(
        self,
        read: ReadDb<'_>,
        write: Option<&WriteDb<'_, I>>,
    ) -> Result<Json, Error> {
        match self {
            Self::Aggregate {
                table,
                filters,
                group_by,
                aggregates,
                having,
                order,
                limit,
            } => {
                read.aggregate(&super::AggregateQuery {
                    table,
                    filters,
                    group_by,
                    aggregates,
                    having,
                    order,
                    limit,
                })
                .await
            }
            Self::Paginate {
                table,
                projection,
                filters,
                order,
                limit,
                cursor,
            } => {
                paginate(
                    read,
                    super::PageQuery {
                        table,
                        projection,
                        filters,
                        order,
                    },
                    limit,
                    cursor.as_deref(),
                    None,
                )
                .await
            }
            Self::Select {
                table,
                projection,
                filters,
                limit,
                order,
            } => {
                let rows = read
                    .select_where_projected(&table, &filters, limit, &order, projection.as_deref())
                    .await?;
                Ok(Json::Array(rows.iter().map(row_json).collect()))
            }
            Self::Insert {
                table,
                values,
                projection,
                conflict,
            } => {
                let write = write.ok_or(Error::Forbidden)?;
                let bulk = values.is_array();
                let inputs = if let Some(values) = values.as_array() {
                    values
                        .iter()
                        .map(|value| decode_record(read, &table, value))
                        .collect::<Result<Vec<_>, _>>()?
                } else {
                    vec![decode_record(read, &table, &values)?]
                };
                let rows = write
                    .insert_many_projected(&table, inputs, projection.as_deref(), conflict.as_ref())
                    .await?;
                if bulk {
                    Ok(Json::Array(rows.iter().map(row_json).collect()))
                } else {
                    Ok(rows.first().map(row_json).unwrap_or(Json::Null))
                }
            }
            Self::Update {
                table,
                projection,
                filters,
                values,
            } => {
                let rows = write
                    .ok_or(Error::Forbidden)?
                    .update_expressions_projected(&table, &filters, values, projection.as_deref())
                    .await?;
                Ok(Json::Array(rows.iter().map(row_json).collect()))
            }
            Self::Delete {
                table,
                filters,
                projection,
            } => {
                let rows = write
                    .ok_or(Error::Forbidden)?
                    .delete_where_projected(&table, &filters, projection.as_deref())
                    .await?;
                Ok(Json::Array(rows.iter().map(row_json).collect()))
            }
        }
    }
}

/// Private wire adapter shared by declared handlers and ordinary server access.
#[doc(hidden)]
pub async fn execute_direct_operation(
    operation: Json,
    read: ReadDb<'_>,
    write: Option<&WriteDb<'_, ()>>,
) -> Result<Json, Error> {
    let operation: Operation = serde_json::from_value(operation)
        .map_err(|_| Error::InvalidInput("invalid database operation".into()))?;
    operation.execute(read, write).await
}

fn decode_record(read: ReadDb<'_>, table: &str, values: &Json) -> Result<RecordValues, Error> {
    let table = read
        .schema()
        .table(table)
        .ok_or_else(|| Error::InvalidInput("unknown table".into()))?;
    values
        .as_object()
        .ok_or_else(|| Error::InvalidInput("expected row fields".into()))?
        .iter()
        .map(|(name, value)| {
            let field = table
                .field(name)
                .ok_or_else(|| Error::InvalidInput("unknown field".into()))?;
            Ok((name.clone(), decode_value(field, value)?))
        })
        .collect()
}
fn decode_value(field: &FieldDefinition, value: &Json) -> Result<Value, Error> {
    if value.is_null() && field.nullable {
        return Ok(Value::Null);
    }
    match field.scalar_type {
        ScalarType::Json => Some(Value::Json(value.clone())),
        ScalarType::Float64 => value.as_number().cloned().map(Value::Float64),
        ScalarType::Date => value.as_str().map(|text| Value::Date(text.into())),
        ScalarType::Timestamp => value.as_str().map(|text| Value::Timestamp(text.into())),
        ScalarType::LocalTimestamp => value
            .as_str()
            .map(|text| Value::LocalTimestamp(text.into())),
        ScalarType::String => value.as_str().map(|text| Value::String(text.into())),
        ScalarType::Boolean => value.as_bool().map(Value::Boolean),
        ScalarType::Int64 => return int64(value).map(Value::Int64),
        ScalarType::Uuid => value
            .as_str()
            .and_then(|text| Uuid::parse_str(text).ok())
            .map(|id| Value::Uuid(*id.as_bytes())),
    }
    .ok_or_else(|| Error::InvalidInput("invalid field value".into()))
}
fn row_json(row: &Row) -> Json {
    let values: BTreeMap<_, _> = row
        .values
        .iter()
        .map(|(name, value)| {
            (
                name,
                match value {
                    Value::Json(value) => value.clone(),
                    Value::Float64(value) => json!(value),
                    Value::Null => Json::Null,
                    Value::Date(text)
                    | Value::Timestamp(text)
                    | Value::LocalTimestamp(text)
                    | Value::String(text) => json!(text),
                    Value::Boolean(value) => json!(value),
                    Value::Int64(value) => json!(value.to_string()),
                    Value::Uuid(value) => json!(Uuid::from_bytes(*value)),
                },
            )
        })
        .collect();
    json!({"row": values, "version": row.version.to_string()})
}

async fn paginate(
    read: ReadDb<'_>,
    query: super::PageQuery,
    limit: u16,
    cursor: Option<&str>,
    requested: Option<&str>,
) -> Result<Json, Error> {
    if limit == 0 || limit > 1000 {
        return Err(Error::InvalidInput("invalid page limit".into()));
    }
    let cursor = cursor.map(super::PageCursor::decode).transpose()?;
    let budget = super::PageBudget {
        max_rows: 1000,
        max_row_json_bytes: 64 * 1024 * 1024,
    };
    let page = if let Some(token) = requested {
        let range = read
            .decode_page_range(&query, token, cursor.as_ref())
            .await?;
        read.refresh_page(&range, budget).await?
    } else {
        read.page(&query, limit, cursor.as_ref(), budget).await?
    };
    let result = json!({"page":page.rows.iter().map(row_json).collect::<Vec<_>>(),"cursor":page.cursor.as_ref().map(super::PageCursor::encode).transpose()?,"range":page.range.encode()?});
    if serde_json::to_vec(&result)
        .map_err(|_| Error::CorruptData)?
        .len()
        > 64 * 1024 * 1024
    {
        return Err(Error::QuotaExceeded);
    }
    Ok(result)
}
