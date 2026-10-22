//! Shared-snapshot evaluation for a captured set of active queries. Delivery,
//! membership fencing and atomic client publication belong to the host.

use std::{collections::BTreeSet, sync::Mutex, time::Duration};

use serde_json::Value as Json;
use tokio_postgres::IsolationLevel;
use uuid::Uuid;

use super::{
    Context, Error, Invocation, ManagedStore, QueryContext, ReadDb, Registry, Revision,
    functions::Handler, store::head,
};

#[derive(Debug, Clone)]
pub struct RoundQuery {
    /// Host-owned member identity, unique within the captured round.
    pub key: String,
    pub operation: String,
    pub input: Json,
    pub page: Option<hulladb_protocol::functions::PageMetadata>,
}

#[derive(Debug, Clone, Copy)]
pub struct RoundLimits {
    pub max_queries: usize,
    /// JSON result payload budget, excluding host/wire framing.
    pub max_result_bytes: usize,
    /// One elapsed budget shared by every member and successful round completion.
    pub timeout: Duration,
}
impl Default for RoundLimits {
    fn default() -> Self {
        Self {
            max_queries: 128,
            max_result_bytes: 16 * 1024 * 1024,
            timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug)]
pub struct RoundResult {
    pub key: String,
    pub result: Result<Json, Error>,
    pub page: Option<hulladb_protocol::functions::PageMetadata>,
    /// Failed handlers conservatively depend on all tables, including failures
    /// before their first read. Empty successful reads still record their table.
    pub tables: BTreeSet<String>,
}

#[derive(Debug)]
pub struct QueryRound {
    pub source: Uuid,
    pub revision: Revision,
    pub principal: Uuid,
    pub authorization_scope: Uuid,
    pub results: Vec<RoundResult>,
    pub result_bytes: usize,
}

impl ManagedStore {
    /// Evaluate a captured active set at one MVCC snapshot. Each member gets a
    /// savepoint so an ordinary SQL error cannot abort successful siblings. Any
    /// failed savepoint cleanup, connection failure, or deadline discards the
    /// complete round; callers must retain their previous coherent view as stale.
    /// On a Tokio deadline, dropping the read-only transaction queues rollback;
    /// it does not establish that cleanup completed before this call returned.
    /// Server statement timeout bounds any still-running SQL.
    /// This does not publish results or establish cross-client coordination.
    pub async fn query_round<I: Send + Sync + 'static>(
        &mut self,
        registry: &Registry<I>,
        context: &Context<I>,
        expected_source: Uuid,
        queries: &[RoundQuery],
        limits: RoundLimits,
    ) -> Result<QueryRound, Error> {
        self.require_initialized()?;
        if limits.max_queries == 0
            || limits.max_queries > 128
            || queries.len() > limits.max_queries
            || limits.max_result_bytes == 0
            || limits.max_result_bytes > 64 * 1024 * 1024
            || limits.timeout.is_zero()
            || limits.timeout > Duration::from_secs(60)
        {
            return Err(Error::InvalidInput(
                "invalid shared query round limits".into(),
            ));
        }
        let mut keys = BTreeSet::new();
        if queries
            .iter()
            .any(|query| query.key.is_empty() || !keys.insert(&query.key))
        {
            return Err(Error::InvalidInput(
                "query round member keys must be nonempty and unique".into(),
            ));
        }
        let deadline = tokio::time::Instant::now() + limits.timeout;
        tokio::time::timeout_at(deadline, async {
            let tx = self.client.build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead).read_only(true).start().await?;
            // Also bound server work if a client-side deadline drops a pending
            // query future. A dropped driver future alone does not cancel SQL.
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            tx.batch_execute(&format!("SET LOCAL statement_timeout = '{}ms'", remaining.as_millis().max(1))).await?;
            let (source, revision) = head(&tx, &self.schema, context.scope, false).await?;
            if source != expected_source { return Err(Error::SourceMismatch); }
            let mut results = Vec::with_capacity(queries.len());
            let mut result_bytes = 0usize;
            for query in queries {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() { return Err(Error::TransactionDeadline); }
                tx.batch_execute(&format!("SET LOCAL statement_timeout = '{}ms'; SAVEPOINT hulla_query_round_member", remaining.as_millis().max(1))).await?;
                let reads = Mutex::new(BTreeSet::new());
                let read_budget = Mutex::new(super::db::ReadBudget::default());
                let db = ReadDb { tx: &tx, schema: &self.schema, scope: context.scope, reads: &reads, read_budget: &read_budget };
                let page = super::pagination::PageExecution {requested:query.page.as_ref().map(|page|page.range.clone()),..Default::default()};
                let result: Result<Json, Error> = async {
                    let function = registry.function(&query.operation)?;
                    let Handler::Query(handler) = &function.handler else { return Err(Error::WrongFunctionKind); };
                    function.contract().validate_input(&query.input)?;
                    if let Some((rows, bytes)) = function.contract().read_limits() { db.set_read_limits(rows, bytes)?; }
                    function.authorization.authorize(QueryContext::new(db, context), Invocation::Query, &query.input).await?;
                    let result = handler.call(QueryContext::new(db, context).with_page(&page), &query.input).await?;
                    db.read_usage()?;
                    function.contract().validate_result(&result)?;
                    if page.requested.is_some() && page.metadata()?.is_none() {return Err(Error::InvalidInput("query does not expose a live page".into()));}
                    Ok(result)
                }.await;
                if result.as_ref().err().is_some_and(infrastructure_failure) {
                    tx.rollback().await?;
                    return Err(match result.expect_err("failed member") {
                        Error::Database(database) if database.code() == Some(&tokio_postgres::error::SqlState::QUERY_CANCELED) => Error::TransactionDeadline,
                        error => error,
                    });
                }
                let tables = if result.is_err() {
                    // Failure here is infrastructure failure, not a member error.
                    tx.batch_execute("ROLLBACK TO SAVEPOINT hulla_query_round_member; RELEASE SAVEPOINT hulla_query_round_member").await?;
                    self.schema.contract().tables().iter().map(|table| table.name().to_owned()).collect()
                } else {
                    tx.batch_execute("RELEASE SAVEPOINT hulla_query_round_member").await?;
                    reads.into_inner().map_err(|_| Error::CorruptData)?
                };
                if let Err(Error::Application(error)) = &result {
                    if !error.valid() { return Err(Error::FunctionExecution); }
                    result_bytes = result_bytes.checked_add(serde_json::to_vec(error).map_err(|_| Error::CorruptData)?.len()).ok_or(Error::QuotaExceeded)?;
                    if result_bytes > limits.max_result_bytes { return Err(Error::QuotaExceeded); }
                }
                if let Ok(result) = &result {
                    result_bytes = result_bytes.checked_add(serde_json::to_vec(result).map_err(|_| Error::CorruptData)?.len()).ok_or(Error::QuotaExceeded)?;
                    if result_bytes > limits.max_result_bytes { return Err(Error::QuotaExceeded); }
                }
                let metadata = if result.is_ok() {page.metadata()?} else {None};
                results.push(RoundResult { key: query.key.clone(), result, page:metadata, tables });
            }
            tx.commit().await?;
            Ok(QueryRound { source, revision, principal: context.principal, authorization_scope: context.authorization_scope, results, result_bytes })
        }).await.map_err(|_| Error::TransactionDeadline)?
    }
}

fn infrastructure_failure(error: &Error) -> bool {
    matches!(
        error,
        Error::SourceMismatch | Error::SchemaMismatch | Error::TransactionDeadline
    ) || matches!(error, Error::Database(database) if database.as_db_error().is_none()
            || database.code() == Some(&tokio_postgres::error::SqlState::QUERY_CANCELED))
}
