//! Durable reusable admission tickets. All times are assigned by PostgreSQL.
//!
//! Tickets admit new requests for five minutes (renew after four). Each accepted
//! request freezes a 15-second start deadline, a 60-second terminal confirmation
//! window, and a 30-second cleanup grace. Execution holds the pending row through
//! business commit; cleanup never fences or purges actively locked work. Ticket
//! deletion precedes marker purge, so moving the clock backwards cannot revive
//! a retired identity. Failure rows retain bounded public codes or intentional application payloads.
//! The scope receipt quota includes tickets, admission tuples and a bounded application-error
//! per-admission reserve for publishing failure after business rollback.
use super::{Context, Error, ManagedSchema, ManagedStore, Receipt, Registry};
use serde_json::Value as Json;
use tokio_postgres::{IsolationLevel, Row, Transaction};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct AdmissionTicket {
    pub id: Uuid,
    pub expires_in_ms: u64,
    pub renew_after_ms: u64,
}
#[derive(Debug, Clone, Default)]
pub struct AdmissionCleanup {
    pub retired_tickets: u64,
    pub compacted_results: u64,
    pub failed_requests: u64,
    pub purged_requests: u64,
}
pub(super) fn ddl(schema: &ManagedSchema) -> String {
    let tickets = schema.relation("tickets");
    let admissions = schema.relation("admissions");
    format!(
        "CREATE TABLE {tickets} (
            id uuid PRIMARY KEY,
            scope uuid NOT NULL,
            principal uuid NOT NULL,
            authorization_scope uuid NOT NULL,
            source uuid NOT NULL,
            expires_at timestamptz NOT NULL
        );
        CREATE INDEX ON {tickets}(expires_at);
        CREATE TABLE {admissions} (
            scope uuid NOT NULL,
            principal uuid NOT NULL,
            request_id uuid NOT NULL,
            ticket uuid NOT NULL,
            authorization_scope uuid NOT NULL,
            source uuid NOT NULL,
            operation text NOT NULL,
            input_digest bytea NOT NULL CHECK(octet_length(input_digest)=32),
            status text NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','success','failure')),
            failure text CHECK((status='failure')=(failure IS NOT NULL)),
            work_until timestamptz NOT NULL,
            result_until timestamptz,
            confirmation_ms bigint NOT NULL DEFAULT 60000,
            grace_ms bigint NOT NULL DEFAULT 30000,
            purge_after timestamptz NOT NULL,
            PRIMARY KEY(scope,principal,request_id)
        );
        CREATE INDEX ON {admissions}(work_until) WHERE status='pending';
        CREATE INDEX ON {admissions}(purge_after) WHERE status<>'pending';
        CREATE INDEX ON {admissions}(result_until)
          WHERE status='success' AND result_until IS NOT NULL;"
    )
}
fn verify<I>(
    row: &Row,
    context: &Context<I>,
    ticket: Uuid,
    source: Uuid,
    payload: Option<(&str, &[u8])>,
) -> Result<(), Error> {
    if row.get::<_, Uuid>("authorization_scope") != context.authorization_scope {
        return Err(Error::Forbidden);
    }
    if row.get::<_, Uuid>("source") != source {
        return Err(Error::SourceMismatch);
    }
    if row.get::<_, Uuid>("ticket") != ticket {
        return Err(Error::RequestIdReused);
    }
    if let Some((operation, digest)) = payload
        && (row.get::<_, String>("operation") != operation
            || row.get::<_, Vec<u8>>("input_digest") != digest)
    {
        return Err(Error::RequestIdReused);
    }
    Ok(())
}
async fn request<I>(
    tx: &Transaction<'_>,
    schema: &ManagedSchema,
    context: &Context<I>,
    request: Uuid,
    lock: bool,
) -> Result<Option<Row>, Error> {
    let admissions = schema.relation("admissions");
    let parameters: &[&(dyn tokio_postgres::types::ToSql + Sync)] =
        &[&context.scope, &context.principal, &request];
    if lock {
        tx.query_opt(
            &format!(
                "SELECT request_id FROM {admissions}
                      WHERE scope=$1 AND principal=$2 AND request_id=$3 FOR UPDATE"
            ),
            parameters,
        )
        .await?;
    }
    // Evaluate the database clock after a potentially blocking lock acquisition.
    Ok(tx
        .query_opt(
            &format!(
                "SELECT *, clock_timestamp()>=work_until AS overdue,
                          COALESCE(clock_timestamp()>=result_until,true) AS result_expired
                   FROM {admissions}
                  WHERE scope=$1 AND principal=$2 AND request_id=$3"
            ),
            parameters,
        )
        .await?)
}
#[allow(clippy::too_many_arguments)]
pub(super) async fn lock_execution<I>(
    tx: &Transaction<'_>,
    schema: &ManagedSchema,
    context: &Context<I>,
    ticket: Uuid,
    request_id: Uuid,
    source: Uuid,
    operation: &str,
    digest: &[u8],
) -> Result<(), Error> {
    let row = request(tx, schema, context, request_id, true)
        .await?
        .ok_or(Error::InvalidTicket)?;
    verify(&row, context, ticket, source, Some((operation, digest)))?;
    match row.get::<_, &str>("status") {
        "failure" => Err(stored_failure(row.get("failure"))?),
        "success" if row.get::<_, bool>("result_expired") => Err(Error::ReceiptExpired),
        "pending" if row.get::<_, bool>("overdue") => Err(Error::TransactionDeadline),
        _ => Ok(()),
    }
}
pub(super) async fn finish_success<I>(
    tx: &Transaction<'_>,
    schema: &ManagedSchema,
    context: &Context<I>,
    request_id: Uuid,
) -> Result<(), Error> {
    tx.execute(
        &format!(
            "UPDATE {}
                     SET status='success',
                         result_until=clock_timestamp()+confirmation_ms*interval '1 millisecond',
                         purge_after=GREATEST(purge_after,
                           clock_timestamp()+(confirmation_ms+grace_ms)*interval '1 millisecond')
                   WHERE scope=$1 AND principal=$2 AND request_id=$3 AND status='pending'",
            schema.relation("admissions")
        ),
        &[&context.scope, &context.principal, &request_id],
    )
    .await?;
    Ok(())
}
fn stored_failure(value: &str) -> Result<Error, Error> {
    if value.starts_with('{') {
        let payload: hulladb_protocol::ApplicationError =
            serde_json::from_str(value).map_err(|_| Error::CorruptData)?;
        if !payload.valid() {
            return Err(Error::CorruptData);
        }
        Ok(Error::Application(payload))
    } else {
        Ok(Error::TerminalFailure(value.to_owned()))
    }
}
fn failure_code(error: &Error) -> &'static str {
    match error {
        Error::Forbidden => "forbidden",
        Error::NotFound => "not_found",
        Error::Conflict | Error::ReferenceViolation => "conflict",
        Error::QuotaExceeded => "quota_exceeded",
        Error::RowExpectationFailed => "row_expectation_failed",
        Error::InvalidInput(_)
        | Error::Validation(_)
        | Error::UnknownFunction(_)
        | Error::WrongFunctionKind => "invalid_request",
        Error::TransactionDeadline | Error::RetryExhausted => "execution_limit",
        _ => "execution_failed",
    }
}
impl ManagedStore {
    pub async fn issue_ticket<I: Send + Sync + 'static>(
        &mut self,
        context: &Context<I>,
    ) -> Result<AdmissionTicket, Error> {
        self.require_initialized()?;
        let tx = self.client.transaction().await?;
        let (source, _) = super::store::head(&tx, &self.schema, context.scope, true).await?;
        let id = Uuid::new_v4();
        tx.execute(
            &format!(
                "INSERT INTO {} VALUES($1,$2,$3,$4,$5,clock_timestamp()+interval '300 seconds')",
                self.schema.relation("tickets")
            ),
            &[
                &id,
                &context.scope,
                &context.principal,
                &context.authorization_scope,
                &source,
            ],
        )
        .await?;
        super::operations::enforce_quota(&tx, &self.schema, context.scope, 0).await?;
        tx.commit().await?;
        Ok(AdmissionTicket {
            id,
            expires_in_ms: 300000,
            renew_after_ms: 240000,
        })
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn mutate_with_ticket<I: Send + Sync + 'static>(
        &mut self,
        registry: &Registry<I>,
        context: &Context<I>,
        ticket: Uuid,
        request_id: Uuid,
        expected_source: Uuid,
        operation: &str,
        input: Json,
    ) -> Result<Receipt, Error> {
        self.require_initialized()?;
        let digest = super::input_digest::input_digest(&input);
        // First admission and native Serializable execution read each other's
        // absent markers. Serializable isolation makes concurrent opposite
        // inserts conflict instead of allowing a late native commit after a
        // durable ticket failure. Every retry starts a fresh transaction.
        let mut attempts = 0;
        let source = loop {
            attempts += 1;
            match self
                .admit_attempt(
                    context,
                    ticket,
                    request_id,
                    expected_source,
                    operation,
                    &digest,
                )
                .await
            {
                Ok(source) => break source,
                Err(error) if attempts < 4 && super::concurrency::retryable(&error) => {
                    tokio::task::yield_now().await;
                }
                Err(error) => return Err(error),
            }
        };
        let result = self
            .mutate_admitted(
                registry,
                context,
                request_id,
                Some(source),
                operation,
                input,
                Some(ticket),
            )
            .await;
        match result {
            Ok(receipt) => Ok(receipt),
            Err(error)
                if matches!(
                    error,
                    Error::CommitOutcomeUnknown
                        | Error::ConnectionDeadline
                        | Error::InvalidTicket
                        | Error::SourceMismatch
                        | Error::RequestIdReused
                        | Error::ReceiptExpired
                        | Error::TerminalFailure(_)
                ) =>
            {
                Err(error)
            }
            Err(error) => {
                let code = match &error {
                    Error::Application(payload) if payload.valid() => {
                        serde_json::to_string(payload).map_err(|_| Error::CorruptData)?
                    }
                    _ => failure_code(&error).to_owned(),
                };
                let tx = self.client.transaction().await?;
                let (current_source, _) =
                    super::store::head(&tx, &self.schema, context.scope, false).await?;
                if current_source != source {
                    return Err(Error::SourceMismatch);
                }
                let row = request(&tx, &self.schema, context, request_id, true)
                    .await?
                    .ok_or(Error::CommitOutcomeUnknown)?;
                verify(&row, context, ticket, source, Some((operation, &digest)))?;
                match row.get::<_, &str>("status") {
                    "pending" => {
                        tx.execute(
                            &format!(
                                "UPDATE {}
                                         SET status='failure', failure=$4,
                                             result_until=clock_timestamp()
                                               +confirmation_ms*interval '1 millisecond',
                                             purge_after=GREATEST(purge_after,
                                               clock_timestamp()+(confirmation_ms+grace_ms)
                                                 *interval '1 millisecond')
                                       WHERE scope=$1 AND principal=$2 AND request_id=$3",
                                self.schema.relation("admissions")
                            ),
                            &[&context.scope, &context.principal, &request_id, &code],
                        )
                        .await?;
                        tx.commit().await?;
                        Err(stored_failure(&code)?)
                    }
                    "failure" => Err(stored_failure(row.get("failure"))?),
                    _ => {
                        tx.commit().await?;
                        self.receipt_with_ticket(context, ticket, request_id, source)
                            .await?
                            .ok_or(Error::CommitOutcomeUnknown)
                    }
                }
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    async fn admit_attempt<I: Send + Sync + 'static>(
        &mut self,
        context: &Context<I>,
        ticket: Uuid,
        request_id: Uuid,
        expected_source: Uuid,
        operation: &str,
        digest: &[u8],
    ) -> Result<Uuid, Error> {
        let tx = self
            .client
            .build_transaction()
            .isolation_level(IsolationLevel::Serializable)
            .start()
            .await?;
        let (source, _) = super::store::head(&tx, &self.schema, context.scope, false).await?;
        if source != expected_source {
            return Err(Error::SourceMismatch);
        }
        if let Some(row) = request(&tx, &self.schema, context, request_id, false).await? {
            verify(&row, context, ticket, source, Some((operation, digest)))?;
        } else {
            // Serialize quota reservations only for first admission. A retained
            // duplicate goes directly to the execution/outcome row lock.
            let (current_source, _) =
                super::store::head(&tx, &self.schema, context.scope, true).await?;
            if current_source != source {
                return Err(Error::SourceMismatch);
            }
            if tx
                .query_opt(
                    &format!(
                        "SELECT 1 FROM {} WHERE scope=$1 AND principal=$2 AND request_id=$3",
                        self.schema.relation("receipts")
                    ),
                    &[&context.scope, &context.principal, &request_id],
                )
                .await?
                .is_some()
            {
                return Err(Error::RequestIdReused);
            }
            let row = tx
                .query_opt(
                    &format!(
                        "SELECT *,id AS ticket FROM {} WHERE id=$1 FOR SHARE",
                        self.schema.relation("tickets")
                    ),
                    &[&ticket],
                )
                .await?
                .ok_or(Error::InvalidTicket)?;
            if row.get::<_, Uuid>("scope") != context.scope
                || row.get::<_, Uuid>("principal") != context.principal
                || row.get::<_, Uuid>("authorization_scope") != context.authorization_scope
            {
                return Err(Error::Forbidden);
            }
            if row.get::<_, Uuid>("source") != source {
                return Err(Error::SourceMismatch);
            }
            // The freshness predicate runs after acquiring the shared ticket
            // lock, held until this immutable admission has committed.
            let count = tx
                .execute(
                    &format!(
                        "INSERT INTO {} (
                    scope, principal, request_id, ticket, authorization_scope,
                    source, operation, input_digest, work_until, purge_after)
                    SELECT $1,$2,$3,$4,$5,$6,$7,$8,
                           clock_timestamp()+interval '15 seconds',
                           GREATEST(expires_at,clock_timestamp()+interval '15 seconds')
                             +interval '30 seconds'
                      FROM {} WHERE id=$4 AND clock_timestamp()<expires_at
                    ON CONFLICT DO NOTHING",
                        self.schema.relation("admissions"),
                        self.schema.relation("tickets")
                    ),
                    &[
                        &context.scope,
                        &context.principal,
                        &request_id,
                        &ticket,
                        &context.authorization_scope,
                        &source,
                        &operation,
                        &digest,
                    ],
                )
                .await?;
            if count > 0 {
                super::operations::enforce_quota(&tx, &self.schema, context.scope, 0).await?;
            }
            if count == 0 {
                let row = request(&tx, &self.schema, context, request_id, false)
                    .await?
                    .ok_or(Error::InvalidTicket)?;
                verify(&row, context, ticket, source, Some((operation, digest)))?;
            }
        }
        tx.commit().await?;
        Ok(source)
    }
    pub async fn receipt_with_ticket<I: Send + Sync + 'static>(
        &mut self,
        context: &Context<I>,
        ticket: Uuid,
        request_id: Uuid,
        expected_source: Uuid,
    ) -> Result<Option<Receipt>, Error> {
        self.require_initialized()?;
        let tx = self.client.transaction().await?;
        let (source, _) = super::store::head(&tx, &self.schema, context.scope, false).await?;
        if source != expected_source {
            return Err(Error::SourceMismatch);
        }
        let Some(row) = request(&tx, &self.schema, context, request_id, false).await? else {
            return Ok(None);
        };
        verify(&row, context, ticket, source, None)?;
        match row.get::<_, &str>("status") {
            "pending" => Ok(None),
            "failure" => Err(stored_failure(row.get("failure"))?),
            _ if row.get::<_, bool>("result_expired") => Err(Error::ReceiptExpired),
            _ => {
                tx.commit().await?;
                self.receipt_from_source(context, request_id, Some(source))
                    .await
            }
        }
    }
    pub async fn acknowledge_ticket<I: Send + Sync + 'static>(
        &mut self,
        context: &Context<I>,
        ticket: Uuid,
        request_id: Uuid,
        source: Uuid,
    ) -> Result<(), Error> {
        self.require_initialized()?;
        let tx = self.client.transaction().await?;
        let (head, _) = super::store::head(&tx, &self.schema, context.scope, false).await?;
        if head != source {
            return Err(Error::SourceMismatch);
        }
        if let Some(row) = request(&tx, &self.schema, context, request_id, true).await? {
            verify(&row, context, ticket, source, None)?;
            tx.execute(
                &format!(
                    "UPDATE {} SET receipt=jsonb_set(receipt,'{{result}}','null'),expired=true
                           WHERE scope=$1 AND principal=$2 AND request_id=$3",
                    self.schema.relation("receipts")
                ),
                &[&context.scope, &context.principal, &request_id],
            )
            .await?;
        }
        tx.execute(
            &format!(
                "UPDATE {} SET result_until=NULL
                       WHERE scope=$1 AND principal=$2 AND request_id=$3 AND status='success'",
                self.schema.relation("admissions")
            ),
            &[&context.scope, &context.principal, &request_id],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn cleanup_admissions(&mut self, limit: u32) -> Result<AdmissionCleanup, Error> {
        self.require_initialized()?;
        if !(1..=1000).contains(&limit) {
            return Err(Error::InvalidInput("cleanup limit must be 1..1000".into()));
        }
        let limit = i64::from(limit);
        let tx = self.client.transaction().await?;
        let tickets = self.schema.relation("tickets");
        let admissions = self.schema.relation("admissions");
        let receipts = self.schema.relation("receipts");
        let compacted_results = tx
            .execute(
                &format!(
                    "WITH selected AS MATERIALIZED (
                SELECT scope,principal,request_id FROM {admissions}
                 WHERE status='success' AND result_until<=clock_timestamp()
                 ORDER BY result_until LIMIT $1 FOR UPDATE SKIP LOCKED
            ), compacted AS (
                UPDATE {receipts} r
                   SET receipt=jsonb_set(receipt,'{{result}}','null'),expired=true
                  FROM selected s
                 WHERE (r.scope,r.principal,r.request_id)=(s.scope,s.principal,s.request_id)
            )
            UPDATE {admissions} a SET result_until=NULL FROM selected s
             WHERE (a.scope,a.principal,a.request_id)=(s.scope,s.principal,s.request_id)"
                ),
                &[&limit],
            )
            .await?;
        let retired_tickets = tx
            .execute(
                &format!(
                    "WITH s AS(SELECT id
                FROM {tickets}
                WHERE expires_at<=clock_timestamp()
                LIMIT $1
                FOR UPDATE SKIP LOCKED) DELETE
                FROM {tickets} t USING s
                WHERE t.id=s.id"
                ),
                &[&limit],
            )
            .await?;
        let failed_requests=tx.execute(&format!("WITH s AS(SELECT scope, principal, request_id
                FROM {admissions}
                WHERE status='pending' AND work_until<=clock_timestamp()
                LIMIT $1
                FOR UPDATE SKIP LOCKED)
                UPDATE {admissions} a
                SET status='failure', failure='execution_limit',
                    result_until=clock_timestamp()+confirmation_ms*interval '1 millisecond',
                    purge_after=GREATEST(purge_after,
                      clock_timestamp()+(confirmation_ms+grace_ms)*interval '1 millisecond')
                FROM s WHERE(a.scope, a.principal, a.request_id)=(s.scope, s.principal, s.request_id)"),&[&limit]).await?;
        let rows = tx
            .query(
                &format!(
                    "SELECT scope, principal, request_id
                FROM {admissions} a
                WHERE status<>'pending' AND purge_after<=clock_timestamp()
                AND NOT EXISTS(SELECT 1
                FROM {tickets} t
                WHERE t.id=a.ticket)
                LIMIT $1
                FOR UPDATE SKIP LOCKED"
                ),
                &[&limit],
            )
            .await?;
        for row in &rows {
            let scope: Uuid = row.get(0);
            let principal: Uuid = row.get(1);
            let request: Uuid = row.get(2);
            for relation in [&receipts, &admissions] {
                tx.execute(
                    &format!(
                        "DELETE FROM {relation} WHERE scope=$1 AND principal=$2 AND request_id=$3"
                    ),
                    &[&scope, &principal, &request],
                )
                .await?;
            }
        }
        tx.commit().await?;
        Ok(AdmissionCleanup {
            retired_tickets,
            compacted_results,
            failed_requests,
            purged_requests: rows.len() as u64,
        })
    }
}
