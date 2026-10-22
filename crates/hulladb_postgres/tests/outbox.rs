use hulladb_postgres::managed::{
    Authorization, ClaimOptions, Context, DeliveryError, DeliveryFuture, DeliveryOutcome,
    EnqueueOptions, Error, FunctionContract, FunctionFuture, Invocation, ManagedSchema,
    ManagedStore, Mutation, MutationContext, OutboxHandler, OutboxJob, OutboxWorker, QueryContext,
    Registry, ScopeQuota, WorkerOptions,
};
use hulladb_schema::{
    FieldDefinition, RecordValues, ScalarType, SchemaDeclaration, TableDeclaration, Value,
};
use serde_json::{Value as Json, json};
use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;

fn url() -> String {
    std::env::var("HULLADB_TEST_URL")
        .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into())
}
struct Permit;
impl Authorization<()> for Permit {
    fn authorize<'a>(
        &'a self,
        _: QueryContext<'a, ()>,
        _: Invocation,
        _: &'a Json,
    ) -> FunctionFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}
struct Enqueue {
    reject_result: bool,
    gate: Option<Arc<tokio::sync::Notify>>,
    started: Option<Arc<tokio::sync::Notify>>,
}
impl FunctionContract for Enqueue {
    fn validate_input(&self, input: &Json) -> Result<(), Error> {
        if input.is_object() {
            Ok(())
        } else {
            Err(Error::InvalidInput("object required".into()))
        }
    }
    fn validate_result(&self, result: &Json) -> Result<(), Error> {
        if !self.reject_result && result.is_array() {
            Ok(())
        } else {
            Err(Error::CorruptData)
        }
    }
}
impl Mutation<()> for Enqueue {
    fn call<'a>(
        &'a self,
        ctx: MutationContext<'a, ()>,
        input: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        Box::pin(async move {
            if input["row"].as_bool().unwrap_or(true) {
                ctx.db
                    .insert(
                        "notes",
                        RecordValues::from([("text".into(), Value::String("committed".into()))]),
                    )
                    .await?;
            }
            let mut ids = Vec::new();
            for _ in 0..input["count"].as_u64().unwrap_or(1) {
                ids.push(
                    ctx.db
                        .enqueue_with(
                            input["name"].as_str().unwrap_or("mail.send"),
                            input
                                .get("payload")
                                .cloned()
                                .unwrap_or(json!({"to":"user","ordinal":i64::MAX.to_string()})),
                            EnqueueOptions {
                                max_attempts: input["max_attempts"].as_u64().unwrap_or(2) as u32,
                                delay: Duration::from_millis(
                                    input["delay_ms"].as_u64().unwrap_or(0),
                                ),
                            },
                        )
                        .await?,
                );
            }
            if let Some(started) = &self.started {
                started.notify_one();
            }
            if let Some(gate) = &self.gate {
                gate.notified().await;
            }
            Ok(json!(ids))
        })
    }
}
fn registry() -> Registry<()> {
    let mut registry = Registry::new();
    for (name, reject_result) in [("enqueue", false), ("fail", true)] {
        registry
            .mutation(
                name,
                Enqueue {
                    reject_result,
                    gate: None,
                    started: None,
                },
                Permit,
            )
            .unwrap();
    }
    registry
}
struct Fixture {
    store: ManagedStore,
    ctx: Context<()>,
    source: Uuid,
}
impl Fixture {
    async fn new() -> Self {
        let contract = SchemaDeclaration::new(vec![TableDeclaration::new(
            "notes",
            vec![FieldDefinition::new("text", ScalarType::String)],
        )])
        .normalize()
        .unwrap();
        let schema =
            ManagedSchema::new(format!("outbox_{}", Uuid::now_v7().simple()), contract).unwrap();
        let mut store = ManagedStore::connect(&url(), schema).await.unwrap();
        store.initialize().await.unwrap();
        let ctx = Context {
            scope: Uuid::now_v7(),
            principal: Uuid::now_v7(),
            authorization_scope: Uuid::now_v7(),
            identity: (),
        };
        store.create_scope(ctx.scope).await.unwrap();
        let source = store.managed_head(&ctx).await.unwrap().0;
        Self { store, ctx, source }
    }
    async fn connection(&self) -> ManagedStore {
        let mut store = ManagedStore::connect(&url(), self.store.schema().clone())
            .await
            .unwrap();
        store.verify().await.unwrap();
        store
    }
    async fn enqueue(&mut self, input: Json) -> hulladb_postgres::managed::Receipt {
        self.store
            .mutate(&registry(), &self.ctx, Uuid::now_v7(), "enqueue", input)
            .await
            .unwrap()
    }
    async fn sql(&self, suffix: &str) {
        let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
            .await
            .unwrap();
        let connection = tokio::spawn(connection);
        client
            .batch_execute(&suffix.replace(
                "$schema",
                &format!("\"{}\"", self.store.schema().namespace()),
            ))
            .await
            .unwrap();
        drop(client);
        connection.await.unwrap().unwrap();
    }
    async fn expire(&self) {
        self.sql("UPDATE $schema.outbox SET lease_until=clock_timestamp()-interval '1 second' WHERE lease_until IS NOT NULL").await;
    }
    async fn cleanup(self) {
        self.sql("DROP SCHEMA $schema CASCADE").await;
    }
}
fn claim() -> ClaimOptions {
    ClaimOptions {
        limit: 1,
        lease: Duration::from_secs(10),
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn outbox_commit_visibility_rollback_and_duplicate_receipts_are_atomic() {
    let mut f = Fixture::new().await;
    let started = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Notify::new());
    let mut registered = registry();
    registered
        .mutation(
            "blocked",
            Enqueue {
                reject_result: false,
                gate: Some(gate.clone()),
                started: Some(started.clone()),
            },
            Permit,
        )
        .unwrap();
    let mut writer = f.connection().await;
    let ctx = f.ctx.clone();
    let request = Uuid::now_v7();
    let task = tokio::spawn(async move {
        writer
            .mutate(&registered, &ctx, request, "blocked", json!({}))
            .await
    });
    started.notified().await;
    assert!(
        f.store
            .claim_outbox(f.source, claim())
            .await
            .unwrap()
            .is_empty(),
        "uncommitted jobs must never be dispatched"
    );
    gate.notify_one();
    let receipt = task.await.unwrap().unwrap();
    let mut same_registration = registry();
    same_registration
        .mutation(
            "blocked",
            Enqueue {
                reject_result: false,
                gate: None,
                started: None,
            },
            Permit,
        )
        .unwrap();
    assert_eq!(
        f.store
            .mutate(&same_registration, &f.ctx, request, "blocked", json!({}))
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(
        f.store.receipt(&f.ctx, request).await.unwrap(),
        Some(receipt.clone())
    );
    let jobs = f
        .store
        .claim_outbox(f.source, ClaimOptions::default())
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1);
    let job = &jobs[0];
    assert_eq!(json!(job.id), receipt.result[0]);
    assert_eq!(job.request_id, request);
    assert_eq!(job.principal, f.ctx.principal);
    assert_eq!(job.authorization_scope, f.ctx.authorization_scope);
    assert_eq!(job.operation, "blocked");
    f.store.acknowledge_outbox(job).await.unwrap();
    // Delivery acknowledgement does not erase mutation recovery or re-enqueue it.
    f.store
        .mutate(&same_registration, &f.ctx, request, "blocked", json!({}))
        .await
        .unwrap();
    assert!(
        f.store
            .claim_outbox(f.source, claim())
            .await
            .unwrap()
            .is_empty()
    );
    let failed = Uuid::now_v7();
    assert!(matches!(
        f.store
            .mutate(&registry(), &f.ctx, failed, "fail", json!({}))
            .await,
        Err(Error::CorruptData)
    ));
    assert!(f.store.receipt(&f.ctx, failed).await.unwrap().is_none());
    let usage = f.store.scope_usage(f.ctx.scope).await.unwrap();
    assert_eq!(usage.rows, 1);
    assert_eq!(usage.outbox_jobs, 0);
    assert_eq!(
        f.store.managed_head(&f.ctx).await.unwrap().1,
        receipt.revision
    );
    let job_only = f.enqueue(json!({"row":false})).await;
    assert_eq!(
        job_only.revision, receipt.revision,
        "private jobs do not advance query-visible revisions"
    );
    assert_eq!(
        f.store.scope_usage(f.ctx.scope).await.unwrap().outbox_jobs,
        1
    );
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn outbox_quotas_and_transaction_budgets_roll_back_jobs_rows_and_receipts() {
    let mut f = Fixture::new().await;
    for quota in [
        ScopeQuota {
            max_outbox_jobs: Some(0),
            ..Default::default()
        },
        ScopeQuota {
            max_outbox_bytes: Some(1),
            ..Default::default()
        },
    ] {
        f.store.set_scope_quota(f.ctx.scope, quota).await.unwrap();
        let request = Uuid::now_v7();
        assert!(matches!(
            f.store
                .mutate(&registry(), &f.ctx, request, "enqueue", json!({}))
                .await,
            Err(Error::QuotaExceeded)
        ));
        assert!(f.store.receipt(&f.ctx, request).await.unwrap().is_none());
        let usage = f.store.scope_usage(f.ctx.scope).await.unwrap();
        assert_eq!(
            (usage.rows, usage.outbox_jobs, usage.outbox_bytes),
            (0, 0, 0)
        );
        assert_eq!(f.store.managed_head(&f.ctx).await.unwrap().1.sequence, 0);
    }
    f.store
        .set_scope_quota(f.ctx.scope, ScopeQuota::default())
        .await
        .unwrap();
    for input in [
        json!({"count":101}),
        json!({"payload":"x".repeat(1024*1024)}),
        json!({"count":2,"payload":"x".repeat(600_000)}),
    ] {
        assert!(matches!(
            f.store
                .mutate(&registry(), &f.ctx, Uuid::now_v7(), "enqueue", input)
                .await,
            Err(Error::QuotaExceeded)
        ));
        assert_eq!(
            f.store.scope_usage(f.ctx.scope).await.unwrap().outbox_jobs,
            0
        );
    }
    for input in [
        json!({"name":"https://external"}),
        json!({"max_attempts":0}),
        json!({"delay_ms":604800001}),
    ] {
        assert!(matches!(
            f.store
                .mutate(&registry(), &f.ctx, Uuid::now_v7(), "enqueue", input)
                .await,
            Err(Error::InvalidInput(_))
        ));
    }
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn concurrent_claims_expired_leases_and_crashed_final_attempts_are_fenced() {
    let mut f = Fixture::new().await;
    f.enqueue(json!({"count":2})).await;
    let mut other = f.connection().await;
    let (a, b) = tokio::join!(
        f.store.claim_outbox(f.source, claim()),
        other.claim_outbox(f.source, claim())
    );
    let a = a.unwrap().pop().unwrap();
    let b = b.unwrap().pop().unwrap();
    assert_ne!(a.id, b.id);
    f.store.acknowledge_outbox(&a).await.unwrap();
    assert!(
        other
            .claim_outbox(f.source, claim())
            .await
            .unwrap()
            .is_empty()
    );
    f.expire().await;
    let next = other
        .claim_outbox(f.source, claim())
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(next.id, b.id);
    assert_eq!(next.attempt, 2);
    assert!(matches!(
        f.store.acknowledge_outbox(&b).await,
        Err(Error::Conflict)
    ));
    assert!(matches!(
        f.store.renew_outbox(&b, Duration::from_secs(10)).await,
        Err(Error::Conflict)
    ));
    assert!(matches!(
        f.store
            .retry_outbox(&b, Duration::ZERO, DeliveryError::Retry("stale".into()))
            .await,
        Err(Error::Conflict)
    ));
    other
        .renew_outbox(&next, Duration::from_secs(20))
        .await
        .unwrap();
    other.acknowledge_outbox(&next).await.unwrap();
    f.enqueue(json!({})).await;
    let first = f
        .store
        .claim_outbox(f.source, claim())
        .await
        .unwrap()
        .pop()
        .unwrap();
    f.expire().await;
    let second = other
        .claim_outbox(f.source, claim())
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!((first.id, second.attempt), (second.id, 2));
    f.expire().await;
    assert!(
        f.store
            .claim_outbox(f.source, claim())
            .await
            .unwrap()
            .is_empty()
    );
    let retained = f.store.inspect_outbox(f.ctx.scope, 100).await.unwrap();
    assert_eq!(retained.len(), 1);
    assert!(retained[0].failed);
    assert_eq!(retained[0].attempts, 2);
    f.store
        .requeue_outbox(second.id, f.source, f.source)
        .await
        .unwrap();
    assert_eq!(
        other.claim_outbox(f.source, claim()).await.unwrap()[0].attempt,
        1
    );
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn scheduled_jobs_retry_delays_dead_letters_and_source_reconciliation_are_explicit() {
    let mut f = Fixture::new().await;
    let receipt = f.enqueue(json!({"delay_ms":60_000})).await;
    assert!(
        f.store
            .claim_outbox(f.source, claim())
            .await
            .unwrap()
            .is_empty()
    );
    f.sql("UPDATE $schema.outbox SET available_at=clock_timestamp()-interval '1 second'")
        .await;
    let job = f
        .store
        .claim_outbox(f.source, claim())
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(
        !f.store
            .retry_outbox(
                &job,
                Duration::from_secs(60),
                DeliveryError::Retry("temporary".into())
            )
            .await
            .unwrap()
    );
    assert!(
        f.store
            .claim_outbox(f.source, claim())
            .await
            .unwrap()
            .is_empty()
    );
    f.sql("UPDATE $schema.outbox SET available_at=clock_timestamp()-interval '1 second'")
        .await;
    let job = f
        .store
        .claim_outbox(f.source, claim())
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(
        f.store
            .retry_outbox(
                &job,
                Duration::ZERO,
                DeliveryError::Permanent("é".repeat(1024))
            )
            .await
            .unwrap()
    );
    let entry = f
        .store
        .inspect_outbox(f.ctx.scope, 100)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(entry.failed);
    assert_eq!(entry.last_error.unwrap().len(), 1024);
    f.store
        .requeue_outbox(job.id, f.source, f.source)
        .await
        .unwrap();
    let old = f
        .store
        .claim_outbox(f.source, claim())
        .await
        .unwrap()
        .pop()
        .unwrap();
    let new_source = f.store.rotate_source(f.source).await.unwrap();
    assert!(matches!(
        f.store.acknowledge_outbox(&old).await,
        Err(Error::SourceMismatch)
    ));
    assert!(matches!(
        f.store.claim_outbox(f.source, claim()).await,
        Err(Error::SourceMismatch)
    ));
    assert!(
        f.store
            .claim_outbox(new_source, claim())
            .await
            .unwrap()
            .is_empty(),
        "a restored source cannot silently redeliver old jobs"
    );
    assert!(matches!(
        f.store.requeue_outbox(old.id, f.source, new_source).await,
        Err(Error::Conflict)
    ));
    f.expire().await;
    f.store
        .requeue_outbox(old.id, f.source, new_source)
        .await
        .unwrap();
    let rebound = f
        .store
        .claim_outbox(new_source, claim())
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(json!(rebound.id), receipt.result[0]);
    assert_eq!(rebound.source, new_source);
    assert!(matches!(
        f.store.acknowledge_outbox(&old).await,
        Err(Error::SourceMismatch)
    ));
    f.store.acknowledge_outbox(&rebound).await.unwrap();
    let abandoned = f.enqueue(json!({"row":false})).await;
    let id = Uuid::parse_str(abandoned.result[0].as_str().unwrap()).unwrap();
    let full = ScopeQuota {
        max_outbox_jobs: Some(0),
        ..Default::default()
    };
    assert!(matches!(
        f.store.set_scope_quota(f.ctx.scope, full.clone()).await,
        Err(Error::QuotaExceeded)
    ));
    assert!(matches!(
        f.store.discard_outbox(id, f.source, new_source).await,
        Err(Error::Conflict)
    ));
    f.store
        .discard_outbox(id, new_source, new_source)
        .await
        .unwrap();
    f.store.set_scope_quota(f.ctx.scope, full).await.unwrap();
    f.cleanup().await;
}

struct IdempotentDelivery {
    calls: AtomicUsize,
    effects: Mutex<HashSet<Uuid>>,
    started: tokio::sync::Notify,
}
impl OutboxHandler for IdempotentDelivery {
    fn deliver<'a>(&'a self, job: &'a OutboxJob) -> DeliveryFuture<'a> {
        Box::pin(async move {
            self.effects.lock().unwrap().insert(job.id);
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.started.notify_one();
                std::future::pending().await
            } else {
                Ok(())
            }
        })
    }
}
#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn worker_timeout_and_cancellation_redeliver_the_same_id_after_an_external_effect() {
    let mut f = Fixture::new().await;
    let receipt = f.enqueue(json!({})).await;
    let options = WorkerOptions {
        lease: Duration::from_secs(10),
        delivery_timeout: Duration::from_millis(30),
        retry_delay: Duration::ZERO,
    };
    let delivery = Arc::new(IdempotentDelivery {
        calls: AtomicUsize::new(0),
        effects: Mutex::new(HashSet::new()),
        started: tokio::sync::Notify::new(),
    });
    let mut worker = OutboxWorker::connect(&url(), f.store.schema().clone(), f.source, options)
        .await
        .unwrap();
    assert_eq!(
        worker.deliver_next(delivery.as_ref()).await.unwrap(),
        DeliveryOutcome::RetryScheduled(
            Uuid::parse_str(receipt.result[0].as_str().unwrap()).unwrap()
        )
    );
    assert!(matches!(
        worker.deliver_next(delivery.as_ref()).await.unwrap(),
        DeliveryOutcome::Delivered(_)
    ));
    assert_eq!(delivery.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        delivery.effects.lock().unwrap().len(),
        1,
        "an idempotent external handler must see one stable job ID"
    );
    assert_eq!(
        worker.deliver_next(delivery.as_ref()).await.unwrap(),
        DeliveryOutcome::Idle
    );
    f.enqueue(json!({})).await;
    delivery.calls.store(0, Ordering::SeqCst);
    // Consume the earlier notification before observing a new started delivery.
    delivery.started.notified().await;
    let work = delivery.clone();
    let task = tokio::spawn(async move { worker.deliver_next(work.as_ref()).await });
    delivery.started.notified().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    f.expire().await;
    let mut restarted = OutboxWorker::connect(&url(), f.store.schema().clone(), f.source, options)
        .await
        .unwrap();
    assert!(matches!(
        restarted.deliver_next(delivery.as_ref()).await.unwrap(),
        DeliveryOutcome::Delivered(_)
    ));
    assert_eq!(delivery.effects.lock().unwrap().len(), 2);
    assert_eq!(
        f.store.scope_usage(f.ctx.scope).await.unwrap().outbox_jobs,
        0
    );
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn incompatible_development_outbox_layouts_are_rejected_without_resetting_data() {
    let mut f = Fixture::new().await;
    f.enqueue(json!({})).await;
    f.sql("ALTER TABLE $schema.quotas DROP COLUMN max_outbox_bytes")
        .await;
    let mut old = ManagedStore::connect(&url(), f.store.schema().clone())
        .await
        .unwrap();
    assert!(matches!(old.verify().await, Err(Error::SchemaMismatch)));
    assert!(matches!(old.initialize().await, Err(Error::SchemaMismatch)));
    // Inspection uses the existing operator connection and does not install or erase data.
    assert_eq!(
        f.store
            .inspect_outbox(f.ctx.scope, 100)
            .await
            .unwrap()
            .len(),
        1
    );
    f.sql("DROP TABLE $schema.outbox").await;
    assert!(matches!(old.verify().await, Err(Error::SchemaMismatch)));
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    assert_eq!(
        client
            .query_one(
                &format!(
                    "SELECT count(*) FROM \"{}\".t0",
                    f.store.schema().namespace()
                ),
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    drop(client);
    connection.await.unwrap().unwrap();
    f.cleanup().await;
}

struct RacingEnqueue {
    barrier: tokio::sync::Barrier,
    calls: AtomicUsize,
}
impl FunctionContract for RacingEnqueue {
    fn validate_input(&self, _: &Json) -> Result<(), Error> {
        Ok(())
    }
    fn validate_result(&self, result: &Json) -> Result<(), Error> {
        if result.is_string() {
            Ok(())
        } else {
            Err(Error::CorruptData)
        }
    }
}
impl Mutation<()> for RacingEnqueue {
    fn call<'a>(&'a self, ctx: MutationContext<'a, ()>, _: &'a Json) -> FunctionFuture<'a, Json> {
        Box::pin(async move {
            ctx.db.select("notes", Default::default()).await?;
            if self.calls.fetch_add(1, Ordering::SeqCst) < 2 {
                self.barrier.wait().await;
            }
            ctx.db
                .insert(
                    "notes",
                    RecordValues::from([("text".into(), Value::String("racing".into()))]),
                )
                .await?;
            Ok(json!(ctx.db.enqueue("notes.created", json!({})).await?))
        })
    }
}
#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn serializable_retries_commit_only_the_successful_attempts_jobs() {
    use hulladb_postgres::managed::{MutationConcurrency, RetryOptions};
    let mut f = Fixture::new().await;
    let mut other = f.connection().await;
    let mut registry = Registry::new();
    registry
        .mutation_with_concurrency(
            "race",
            RacingEnqueue {
                barrier: tokio::sync::Barrier::new(2),
                calls: AtomicUsize::new(0),
            },
            Permit,
            MutationConcurrency::Serializable(RetryOptions {
                max_attempts: 3,
                timeout: Duration::from_secs(3),
                backoff: Duration::from_millis(1),
            }),
        )
        .unwrap();
    let (a, b) = tokio::join!(
        f.store
            .mutate(&registry, &f.ctx, Uuid::now_v7(), "race", Json::Null),
        other.mutate(&registry, &f.ctx, Uuid::now_v7(), "race", Json::Null),
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a.revision.sequence, b.revision.sequence);
    let jobs = f
        .store
        .claim_outbox(f.source, ClaimOptions::default())
        .await
        .unwrap();
    assert_eq!(
        jobs.len(),
        2,
        "rolled-back attempts must not leave deliverable jobs"
    );
    let delivered: HashSet<_> = jobs.iter().map(|job| job.id.to_string()).collect();
    assert_eq!(
        delivered,
        [
            a.result.as_str().unwrap().to_owned(),
            b.result.as_str().unwrap().to_owned()
        ]
        .into()
    );
    let usage = f.store.scope_usage(f.ctx.scope).await.unwrap();
    assert_eq!((usage.rows, usage.outbox_jobs), (2, 2));
    f.cleanup().await;
}
