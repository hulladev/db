use std::{
    collections::BTreeSet,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde_json::Value as Json;
use tokio::task::JoinHandle;
use tokio_postgres::{Client, IsolationLevel, NoTls, Transaction};
use uuid::Uuid;

use super::{
    Context, Error, Invocation, ManagedSchema, MutationConcurrency, MutationContext, QueryContext,
    ReadDb, Receipt, Registry, Revision, Snapshot, WriteDb, concurrency::retryable,
    functions::Handler,
};

pub struct ManagedStore {
    pub(super) client: Client,
    connection: JoinHandle<Result<(), tokio_postgres::Error>>,
    pub(super) schema: ManagedSchema,
    initialized: bool,
}

impl Drop for ManagedStore {
    fn drop(&mut self) {
        self.connection.abort();
    }
}

impl ManagedStore {
    /// Server configuration owns credentials. Remote connections require
    /// sslmode=require (verified TLS), or an explicit sslmode=disable for a
    /// private network. No tables are installed implicitly.
    pub async fn connect(url: &str, schema: ManagedSchema) -> Result<Self, Error> {
        Self::connect_config(url.parse()?, schema).await
    }

    pub async fn connect_config(
        config: tokio_postgres::Config,
        schema: ManagedSchema,
    ) -> Result<Self, Error> {
        tokio::time::timeout(Duration::from_secs(10), Self::connect_inner(config, schema))
            .await
            .map_err(|_| Error::ConnectionDeadline)?
    }

    async fn connect_inner(
        mut config: tokio_postgres::Config,
        schema: ManagedSchema,
    ) -> Result<Self, Error> {
        config.connect_timeout(Duration::from_secs(5));
        let (client, connection) = if config.get_ssl_mode()
            == tokio_postgres::config::SslMode::Require
        {
            let (client, connection) = config.connect(super::tls::verified_tls()).await?;
            (client, tokio::spawn(connection))
        } else {
            if config.get_ssl_mode() != tokio_postgres::config::SslMode::Disable
                && config.get_hosts().iter().any(|host| match host {
                    tokio_postgres::config::Host::Tcp(host) => {
                        host != "localhost"
                            && !host
                                .parse::<std::net::IpAddr>()
                                .is_ok_and(|ip| ip.is_loopback())
                    }
                    #[cfg(unix)]
                    tokio_postgres::config::Host::Unix(_) => false,
                })
            {
                return Err(Error::InvalidInput("remote PostgreSQL requires verified sslmode=require or explicit private-network sslmode=disable".into()));
            }
            let (client, connection) = config.connect(NoTls).await?;
            (client, tokio::spawn(connection))
        };
        let store = Self {
            client,
            connection,
            schema,
            initialized: false,
        };
        store
            .client
            .batch_execute("SET statement_timeout = '5s'; SET lock_timeout = '5s';")
            .await?;
        Ok(store)
    }

    pub fn is_closed(&self) -> bool {
        self.client.is_closed()
    }
    pub fn schema(&self) -> &ManagedSchema {
        &self.schema
    }

    /// Install a fresh layout or verify the exact existing contract. This never
    /// alters an existing schema or erases incompatible development data.
    pub async fn initialize(&mut self) -> Result<(), Error> {
        self.check_layout(true).await
    }

    /// Verify existing storage without installing tables during serving.
    pub async fn verify(&mut self) -> Result<(), Error> {
        self.check_layout(false).await
    }

    async fn check_layout(&mut self, install: bool) -> Result<(), Error> {
        self.initialized = false;
        let tx = self.client.transaction().await?;
        // Serializes installation only; hash collisions merely serialize two
        // independent installations. Mutations use the scope row lock below.
        tx.execute(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            &[&self.schema.namespace],
        )
        .await?;
        let exists: bool = tx
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname=$1)",
                &[&self.schema.namespace],
            )
            .await?
            .get(0);
        if exists {
            let metadata_exists: bool = tx
                .query_one(
                    "SELECT to_regclass($1) IS NOT NULL AND to_regclass($2) IS NOT NULL AND to_regclass($3) IS NOT NULL",
                    &[
                        &self.schema.relation("metadata"),
                        &self.schema.relation("quotas"),
                        &self.schema.relation("outbox"),
                    ],
                )
                .await?
                .get(0);
            if !metadata_exists {
                return Err(Error::SchemaMismatch);
            }
            super::outbox::verify_layout(&tx, &self.schema).await?;
            let sql = format!(
                "SELECT contract FROM {} WHERE singleton",
                self.schema.relation("metadata")
            );
            let descriptor = tx
                .query_opt(&sql, &[])
                .await?
                .ok_or(Error::SchemaMismatch)?
                .try_get::<_, String>(0)?;
            let descriptor: Json =
                serde_json::from_str(&descriptor).map_err(|_| Error::SchemaMismatch)?;
            if !super::storage::descriptor_equal(&descriptor, &self.schema.descriptor()) {
                return Err(Error::SchemaMismatch);
            }
        } else {
            if !install {
                return Err(Error::SchemaMismatch);
            }
            tx.batch_execute(&self.schema.ddl()?).await?;
            tx.execute(
                &format!(
                    "INSERT INTO {} VALUES (true,$1,$2)",
                    self.schema.relation("metadata")
                ),
                &[&Uuid::now_v7(), &self.schema.descriptor().to_string()],
            )
            .await?;
        }
        tx.commit().await?;
        self.initialized = true;
        Ok(())
    }

    /// Trusted provisioning; not an authorization grant or a client callable.
    pub async fn create_scope(&mut self, scope: Uuid) -> Result<(), Error> {
        self.require_initialized()?;
        self.client
            .execute(
                &format!(
                    "INSERT INTO {} (id) VALUES ($1)",
                    self.schema.relation("scopes")
                ),
                &[&scope],
            )
            .await?;
        Ok(())
    }

    pub async fn query<I: Send + Sync + 'static>(
        &mut self,
        registry: &Registry<I>,
        context: &Context<I>,
        operation: &str,
        input: Json,
    ) -> Result<Snapshot, Error> {
        self.require_initialized()?;
        let function = registry.function(operation)?;
        let Handler::Query(handler) = &function.handler else {
            return Err(Error::WrongFunctionKind);
        };
        function.contract().validate_input(&input)?;
        let tx = self
            .client
            .build_transaction()
            .isolation_level(IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .await?;
        let reads = Mutex::new(BTreeSet::new());
        let read_budget = Mutex::new(super::db::ReadBudget::default());
        let db = ReadDb {
            tx: &tx,
            schema: &self.schema,
            scope: context.scope,
            reads: &reads,
            read_budget: &read_budget,
        };
        if let Some((rows, bytes)) = function.contract().read_limits() {
            db.set_read_limits(rows, bytes)?;
        }
        let (source, revision) = head(&tx, &self.schema, context.scope, false).await?;
        function
            .authorization
            .authorize(QueryContext::new(db, context), Invocation::Query, &input)
            .await?;
        let page = super::pagination::PageExecution::default();
        let result = handler
            .call(QueryContext::new(db, context).with_page(&page), &input)
            .await?;
        db.read_usage()?;
        function.contract().validate_result(&result)?;
        tx.commit().await?;
        let snapshot = Snapshot {
            source,
            revision,
            principal: context.principal,
            authorization_scope: context.authorization_scope,
            result,
            page: page.metadata()?,
            tables: reads.into_inner().map_err(|_| Error::CorruptData)?,
        };
        Ok(snapshot)
    }

    pub async fn mutate<I: Send + Sync + 'static>(
        &mut self,
        registry: &Registry<I>,
        context: &Context<I>,
        request_id: Uuid,
        operation: &str,
        input: Json,
    ) -> Result<Receipt, Error> {
        self.mutate_from_source(registry, context, request_id, None, operation, input)
            .await
    }

    /// Reject replay across a replaced source inside the locked mutation transaction.
    pub async fn mutate_from_source<I: Send + Sync + 'static>(
        &mut self,
        registry: &Registry<I>,
        context: &Context<I>,
        request_id: Uuid,
        expected_source: Option<Uuid>,
        operation: &str,
        input: Json,
    ) -> Result<Receipt, Error> {
        self.mutate_admitted(
            registry,
            context,
            request_id,
            expected_source,
            operation,
            input,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn mutate_admitted<I: Send + Sync + 'static>(
        &mut self,
        registry: &Registry<I>,
        context: &Context<I>,
        request_id: Uuid,
        expected_source: Option<Uuid>,
        operation: &str,
        input: Json,
        ticket: Option<Uuid>,
    ) -> Result<Receipt, Error> {
        self.require_initialized()?;
        let input_digest = super::input_digest::input_digest(&input);
        // A committed response belongs to its original invocation, even after a
        // deployment removes or changes the declaration. Resolve/validate the
        // current contract only after checking the stored outcome in the attempt.
        let concurrency = registry
            .function(operation)
            .map_or(MutationConcurrency::default(), |function| {
                function.concurrency
            });
        if matches!(concurrency, MutationConcurrency::Scope) {
            return self
                .mutate_attempt(
                    registry,
                    context,
                    request_id,
                    expected_source,
                    operation,
                    &input,
                    &input_digest,
                    concurrency,
                    &AtomicBool::new(false),
                    ticket,
                )
                .await
                .map_err(|failure| failure.error);
        }
        let MutationConcurrency::Serializable(options) = concurrency else {
            unreachable!()
        };
        let deadline = tokio::time::Instant::now() + options.timeout;
        for attempt in 1..=options.max_attempts {
            let commit_started = AtomicBool::new(false);
            let result = tokio::time::timeout_at(
                deadline,
                self.mutate_attempt(
                    registry,
                    context,
                    request_id,
                    expected_source,
                    operation,
                    &input,
                    &input_digest,
                    concurrency,
                    &commit_started,
                    ticket,
                ),
            )
            .await;
            let error = match result {
                Ok(Ok(receipt)) => return Ok(receipt),
                Ok(Err(failure)) if failure.rolled_back => failure.error,
                Ok(Err(failure)) => return Err(failure.error),
                Err(_) if commit_started.load(Ordering::Acquire) => {
                    return Err(Error::CommitOutcomeUnknown);
                }
                Err(_) => return Err(Error::TransactionDeadline),
            };
            if !retryable(&error) && !matches!(error, Error::Conflict) {
                return Err(error);
            }
            // A duplicate may lose a unique/serialization race. Recover through
            // a fresh snapshot without replaying its handler, even with one attempt.
            let recovered = tokio::time::timeout_at(
                deadline,
                self.receipt_checked(
                    context,
                    request_id,
                    expected_source,
                    Some((operation, &input_digest)),
                ),
            )
            .await
            .map_err(|_| Error::TransactionDeadline)??;
            if let Some(receipt) = recovered {
                return Ok(receipt);
            }
            if !retryable(&error) {
                return Err(error);
            }
            if attempt == options.max_attempts {
                return Err(Error::RetryExhausted);
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(Error::TransactionDeadline);
            }
            let factor = 1u32 << (attempt - 1).min(8);
            let jitter = u32::from(Uuid::new_v4().as_bytes()[0]) + 128;
            let delay = options
                .backoff
                .saturating_mul(factor)
                .saturating_mul(jitter)
                / 256;
            if delay >= remaining {
                return Err(Error::TransactionDeadline);
            }
            tokio::time::sleep(delay).await;
        }
        unreachable!("registration validates the attempt budget")
    }

    #[allow(clippy::too_many_arguments)]
    async fn mutate_attempt<I: Send + Sync + 'static>(
        &mut self,
        registry: &Registry<I>,
        context: &Context<I>,
        request_id: Uuid,
        expected_source: Option<Uuid>,
        operation: &str,
        input: &Json,
        input_digest: &[u8],
        concurrency: MutationConcurrency,
        commit_started: &AtomicBool,
        ticket: Option<Uuid>,
    ) -> Result<Receipt, AttemptFailure> {
        let tx = match concurrency {
            MutationConcurrency::Scope => self.client.transaction().await?,
            MutationConcurrency::Serializable(_) => {
                self.client
                    .build_transaction()
                    .isolation_level(IsolationLevel::Serializable)
                    .start()
                    .await?
            }
        };
        let result: Result<Receipt, Error> = async {
            let (source, mut revision) = head(
                &tx,
                &self.schema,
                context.scope,
                matches!(concurrency, MutationConcurrency::Scope),
            )
            .await?;
            if expected_source.is_some_and(|expected| expected != source) {
                return Err(Error::SourceMismatch);
            }
            if ticket.is_none() && tx.query_opt(&format!("SELECT 1 FROM {} WHERE scope=$1 AND principal=$2 AND request_id=$3",self.schema.relation("admissions")),&[&context.scope,&context.principal,&request_id]).await?.is_some() {
                return Err(Error::RequestIdReused);
            }
            if let Some(ticket) = ticket {
                super::admission::lock_execution(&tx, &self.schema, context, ticket, request_id, source, operation, input_digest).await?;
            }
            if let Some(stored) = stored_receipt(&tx, &self.schema, context, request_id).await? {
                verify_receipt_head(&stored.receipt, source, revision)?;
                if stored.authorization_scope != context.authorization_scope {
                    return Err(Error::Forbidden);
                }
                if stored.expired {
                    return Err(Error::ReceiptExpired);
                }
                if stored.operation != operation || stored.input_digest != input_digest {
                    return Err(Error::RequestIdReused);
                }
                if ticket.is_some() {
                    // A trusted native execution may have committed between
                    // first admission and execution. Adopt that same immutable
                    // success atomically rather than leaving a pending marker.
                    super::admission::finish_success(&tx,&self.schema,context,request_id).await?;
                }
                return Ok(stored.receipt);
            }
            let function = registry.function(operation)?;
            let Handler::Mutation(handler) = &function.handler else {
                return Err(Error::WrongFunctionKind);
            };
            function.contract().validate_input(input)?;
            let reads = Mutex::new(BTreeSet::new());
            let read_budget = Mutex::new(super::db::ReadBudget::default());
            let db = ReadDb {
                tx: &tx,
                schema: &self.schema,
                scope: context.scope,
                reads: &reads,
                read_budget: &read_budget,
            };
            if let Some((rows, bytes)) = function.contract().read_limits() { db.set_read_limits(rows, bytes)?; }
            function
                .authorization
                .authorize(QueryContext::new(db, context), Invocation::Mutation, input)
                .await?;
            let dirty = AtomicBool::new(false);
            let outbox = super::outbox::EnqueueContext::new(source, request_id, operation);
            let result = handler
                .call(
                    MutationContext {
                        db: WriteDb {
                            read: db,
                            context,
                            registry,
                            dirty: &dirty,
                            outbox: &outbox,
                        },
                        identity: &context.identity,
                    },
                    input,
                )
                .await?;
            db.read_usage()?;
        function.contract().validate_result(&result)?;
            if dirty.load(Ordering::Relaxed) {
                revision.sequence = advance_revision(&tx, &self.schema, context.scope).await?;
            }
            let receipt = Receipt {
                request_id,
                source,
                revision,
                result,
            };
            let encoded = serde_json::to_value(&receipt).map_err(|_| Error::CorruptData)?;
            tx.execute(
                &format!(
                    "INSERT INTO {} (scope,principal,request_id,authorization_scope,operation,input_digest,receipt) VALUES ($1,$2,$3,$4,$5,$6,$7)",
                    self.schema.relation("receipts")
                ),
                &[
                    &context.scope,
                    &context.principal,
                    &request_id,
                    &context.authorization_scope,
                    &operation,
                    &input_digest,
                    &encoded,
                ],
            )
            .await?;
            let result_bytes = i64::try_from(
                serde_json::to_vec(&receipt.result)
                    .map_err(|_| Error::CorruptData)?
                    .len(),
            )
            .map_err(|_| Error::CorruptData)?;
            if ticket.is_some() {
                super::admission::finish_success(&tx, &self.schema, context, request_id).await?;
            }
            super::operations::enforce_quota(&tx, &self.schema, context.scope, result_bytes).await?;
            Ok(receipt)
        }.await;
        match result {
            Ok(receipt) => {
                commit_started.store(true, Ordering::Release);
                tx.commit().await.map_err(|error| AttemptFailure {
                    rolled_back: error.as_db_error().is_some(),
                    error: commit_error(error),
                })?;
                Ok(receipt)
            }
            Err(error) => {
                // Dropping a transaction only queues rollback. Await the server's
                // acknowledgement before permitting any whole-handler replay.
                tx.rollback().await?;
                Err(AttemptFailure {
                    error,
                    rolled_back: true,
                })
            }
        }
    }

    /// Deliver the original authorized response without rerunning application code.
    /// The caller must provide a currently verified context. Stored ownership,
    /// authentication scope and source fences still govern delivery; current
    /// function registration, validators and database permissions do not.
    pub async fn receipt<I: Send + Sync + 'static>(
        &mut self,
        context: &Context<I>,
        request_id: Uuid,
    ) -> Result<Option<Receipt>, Error> {
        self.receipt_from_source(context, request_id, None).await
    }

    pub async fn receipt_from_source<I: Send + Sync + 'static>(
        &mut self,
        context: &Context<I>,
        request_id: Uuid,
        expected_source: Option<Uuid>,
    ) -> Result<Option<Receipt>, Error> {
        self.receipt_checked(context, request_id, expected_source, None)
            .await
    }

    async fn receipt_checked<I: Send + Sync + 'static>(
        &mut self,
        context: &Context<I>,
        request_id: Uuid,
        expected_source: Option<Uuid>,
        expected_request: Option<(&str, &[u8])>,
    ) -> Result<Option<Receipt>, Error> {
        self.require_initialized()?;
        let tx = self
            .client
            .build_transaction()
            .isolation_level(IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .await?;
        let (source, revision) = head(&tx, &self.schema, context.scope, false).await?;
        if expected_source.is_some_and(|expected| expected != source) {
            return Err(Error::SourceMismatch);
        }
        let Some(stored) = stored_receipt(&tx, &self.schema, context, request_id).await? else {
            tx.commit().await?;
            return Ok(None);
        };
        verify_receipt_head(&stored.receipt, source, revision)?;
        if stored.authorization_scope != context.authorization_scope {
            return Err(Error::Forbidden);
        }
        if stored.expired {
            return Err(Error::ReceiptExpired);
        }
        if let Some((operation, input_digest)) = expected_request
            && (stored.operation != operation || stored.input_digest != input_digest)
        {
            return Err(Error::RequestIdReused);
        }
        tx.commit().await?;
        Ok(Some(stored.receipt))
    }

    /// Authenticated deployment identity/fence; this grants no row access.
    pub async fn managed_head<I>(
        &mut self,
        context: &Context<I>,
    ) -> Result<(Uuid, Revision), Error> {
        self.require_initialized()?;
        let tx = self
            .client
            .build_transaction()
            .isolation_level(IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .await?;
        let value = head(&tx, &self.schema, context.scope, false).await?;
        tx.commit().await?;
        Ok(value)
    }

    pub(super) fn require_initialized(&self) -> Result<(), Error> {
        if self.initialized {
            Ok(())
        } else {
            Err(Error::InvalidInput(
                "initialize the managed schema before execution".into(),
            ))
        }
    }
}

pub(super) async fn head(
    tx: &Transaction<'_>,
    schema: &ManagedSchema,
    scope: Uuid,
    lock: bool,
) -> Result<(Uuid, Revision), Error> {
    let sql = format!(
        "SELECT sequence FROM {} WHERE id=$1{}",
        schema.relation("scopes"),
        if lock { " FOR NO KEY UPDATE" } else { "" }
    );
    let sequence: i64 = tx
        .query_opt(&sql, &[&scope])
        .await?
        .ok_or(Error::NotFound)?
        .try_get(0)?;
    let source: Uuid = tx
        .query_one(
            &format!(
                "SELECT source FROM {} WHERE singleton",
                schema.relation("metadata")
            ),
            &[],
        )
        .await?
        .try_get(0)?;
    if sequence < 0 {
        return Err(Error::CorruptData);
    }
    Ok((source, Revision { scope, sequence }))
}

struct StoredReceipt {
    authorization_scope: Uuid,
    operation: String,
    input_digest: Vec<u8>,
    receipt: Receipt,
    expired: bool,
}

async fn stored_receipt<I>(
    tx: &Transaction<'_>,
    schema: &ManagedSchema,
    context: &Context<I>,
    request_id: Uuid,
) -> Result<Option<StoredReceipt>, Error> {
    let row = tx.query_opt(&format!("SELECT authorization_scope, operation, input_digest, receipt, expired FROM {} WHERE scope=$1 AND principal=$2 AND request_id=$3", schema.relation("receipts")),
        &[&context.scope, &context.principal, &request_id]).await?;
    row.map(|row| {
        let encoded: Json = row.try_get(3)?;
        let receipt: Receipt = serde_json::from_value(encoded).map_err(|_| Error::CorruptData)?;
        if receipt.request_id != request_id
            || receipt.revision.scope != context.scope
            || receipt.revision.sequence < 0
        {
            return Err(Error::CorruptData);
        }
        let input_digest: Vec<u8> = row.try_get(2)?;
        if input_digest.len() != 32 {
            return Err(Error::CorruptData);
        }
        Ok(StoredReceipt {
            authorization_scope: row.try_get(0)?,
            operation: row.try_get(1)?,
            input_digest,
            receipt,
            expired: row.try_get(4)?,
        })
    })
    .transpose()
}

pub(super) fn commit_error(error: tokio_postgres::Error) -> Error {
    // A server error confirms commit rejection (for example a deferred
    // constraint). A transport failure cannot establish whether COMMIT ran.
    if error.as_db_error().is_some() {
        error.into()
    } else {
        Error::CommitOutcomeUnknown
    }
}

fn verify_receipt_head(receipt: &Receipt, source: Uuid, revision: Revision) -> Result<(), Error> {
    if receipt.source != source {
        return Err(Error::SourceMismatch);
    }
    if receipt.revision.sequence > revision.sequence {
        return Err(Error::CorruptData);
    }
    Ok(())
}

/// An attempt may only be replayed once PostgreSQL confirms its rollback.
struct AttemptFailure {
    error: Error,
    rolled_back: bool,
}
impl From<Error> for AttemptFailure {
    fn from(error: Error) -> Self {
        Self {
            error,
            rolled_back: false,
        }
    }
}
impl From<tokio_postgres::Error> for AttemptFailure {
    fn from(error: tokio_postgres::Error) -> Self {
        Error::from(error).into()
    }
}

// Direct server writes and declared mutations publish the same scope revision.
pub(super) async fn advance_revision(
    tx: &Transaction<'_>,
    schema: &ManagedSchema,
    scope: Uuid,
) -> Result<i64, Error> {
    Ok(tx.query_one(
        &format!("UPDATE {} SET sequence=sequence+1 WHERE id=$1 AND sequence < 9223372036854775807 RETURNING sequence", schema.relation("scopes")),
        &[&scope],
    ).await?.try_get(0)?)
}
