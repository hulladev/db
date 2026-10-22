use hulladb_postgres::managed::{
    Authorization, Context, Error, FunctionContract, FunctionFuture, Invocation, ManagedSchema,
    ManagedStore, Mutation, MutationContext, Registry, Roles, ScopeQuota,
};
use hulladb_schema::{
    FieldDefinition, RecordValues, ScalarType, SchemaDeclaration, TableDeclaration, Value,
};
use serde_json::{Value as Json, json};
use uuid::Uuid;

fn url() -> String {
    std::env::var("HULLADB_TEST_URL")
        .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into())
}
struct Permit;
impl Authorization<()> for Permit {
    fn authorize<'a>(
        &'a self,
        _: hulladb_postgres::managed::QueryContext<'a, ()>,
        _: Invocation,
        _: &'a Json,
    ) -> FunctionFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}
struct Insert;
impl FunctionContract for Insert {
    fn validate_input(&self, input: &Json) -> Result<(), Error> {
        if input.is_string() {
            Ok(())
        } else {
            Err(Error::InvalidInput("text required".into()))
        }
    }
    fn validate_result(&self, result: &Json) -> Result<(), Error> {
        if result.is_string() {
            Ok(())
        } else {
            Err(Error::CorruptData)
        }
    }
}
impl Mutation<()> for Insert {
    fn call<'a>(
        &'a self,
        context: MutationContext<'a, ()>,
        input: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        Box::pin(async move {
            context.db.enqueue("notes.created", input.clone()).await?;
            context
                .db
                .insert(
                    "notes",
                    RecordValues::from([(
                        "text".into(),
                        Value::String(input.as_str().unwrap().into()),
                    )]),
                )
                .await?;
            Ok(input.clone())
        })
    }
}
async fn fixture() -> (ManagedStore, Registry<()>, Context<()>) {
    let schema = SchemaDeclaration::new(vec![TableDeclaration::new(
        "notes",
        vec![FieldDefinition::new("text", ScalarType::String)],
    )])
    .normalize()
    .unwrap();
    let schema = ManagedSchema::new(format!("ops_{}", Uuid::now_v7().simple()), schema).unwrap();
    let mut store = ManagedStore::connect(&url(), schema).await.unwrap();
    store.initialize().await.unwrap();
    let context = Context {
        scope: Uuid::now_v7(),
        principal: Uuid::now_v7(),
        authorization_scope: Uuid::now_v7(),
        identity: (),
    };
    store.create_scope(context.scope).await.unwrap();
    let mut registry = Registry::new();
    registry.mutation("insert", Insert, Permit).unwrap();
    (store, registry, context)
}
async fn cleanup(store: &ManagedStore) {
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!(
            "DROP SCHEMA \"{}\" CASCADE",
            store.schema().namespace()
        ))
        .await
        .unwrap();
    connection.abort();
}
#[tokio::test]
#[ignore = "requires PostgreSQL; run make production-test"]
async fn quotas_roll_back_rows_revision_and_receipt_but_allow_duplicate_recovery() {
    let (mut store, registry, context) = fixture().await;
    store
        .set_scope_quota(
            context.scope,
            ScopeQuota {
                max_rows: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let request = Uuid::now_v7();
    let first = store
        .mutate(&registry, &context, request, "insert", json!("one"))
        .await
        .unwrap();
    let rejected = Uuid::now_v7();
    assert!(matches!(
        store
            .mutate(&registry, &context, rejected, "insert", json!("two"))
            .await,
        Err(Error::QuotaExceeded)
    ));
    assert_eq!(store.scope_usage(context.scope).await.unwrap().rows, 1);
    assert_eq!(
        store.managed_head(&context).await.unwrap().1,
        first.revision
    );
    assert!(store.receipt(&context, rejected).await.unwrap().is_none());
    assert_eq!(
        store
            .mutate(&registry, &context, request, "insert", json!("one"))
            .await
            .unwrap(),
        first
    );
    assert!(matches!(
        store
            .set_scope_quota(
                context.scope,
                ScopeQuota {
                    max_rows: Some(0),
                    ..Default::default()
                }
            )
            .await,
        Err(Error::QuotaExceeded)
    ));
    cleanup(&store).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL; run make production-test"]
async fn result_data_and_receipt_byte_limits_fail_before_commit() {
    let (mut store, registry, context) = fixture().await;
    for quota in [
        ScopeQuota {
            max_result_bytes: Some(2),
            ..Default::default()
        },
        ScopeQuota {
            max_data_bytes: Some(0),
            ..Default::default()
        },
        ScopeQuota {
            max_receipt_bytes: Some(0),
            ..Default::default()
        },
    ] {
        store.set_scope_quota(context.scope, quota).await.unwrap();
        let request = Uuid::now_v7();
        assert!(matches!(
            store
                .mutate(&registry, &context, request, "insert", json!("large"))
                .await,
            Err(Error::QuotaExceeded)
        ));
        assert_eq!(store.scope_usage(context.scope).await.unwrap().rows, 0);
        assert_eq!(store.managed_head(&context).await.unwrap().1.sequence, 0);
        assert!(store.receipt(&context, request).await.unwrap().is_none());
    }
    cleanup(&store).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL; run make production-test"]
async fn role_provisioning_separates_runtime_worker_operator_and_backup_privileges() {
    let (mut store, registry, context) = fixture().await;
    let suffix = Uuid::now_v7().simple().to_string();
    let roles = Roles {
        worker: format!("wrk_{suffix}"),
        owner: format!("own_{suffix}"),
        runtime: format!("run_{suffix}"),
        backup: format!("bak_{suffix}"),
    };
    store.provision_roles(&roles).await.unwrap();
    store.provision_roles(&roles).await.unwrap();
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let login = format!("login_{suffix}");
    let password = Uuid::now_v7().simple().to_string();
    client.batch_execute(&format!("CREATE ROLE \"{login}\" LOGIN PASSWORD '{password}' NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS")).await.unwrap();
    store.grant_role(&roles.runtime, &login).await.unwrap();
    let mut config: tokio_postgres::Config = url().parse().unwrap();
    config.user(&login).password(&password);
    let mut runtime = ManagedStore::connect_config(config.clone(), store.schema().clone())
        .await
        .unwrap();
    runtime.verify().await.unwrap();
    runtime
        .mutate(
            &registry,
            &context,
            Uuid::now_v7(),
            "insert",
            json!("permitted"),
        )
        .await
        .unwrap();
    assert!(runtime.create_scope(Uuid::now_v7()).await.is_err());
    assert!(
        runtime
            .set_scope_quota(context.scope, ScopeQuota::default())
            .await
            .is_err()
    );
    assert!(matches!(
        runtime.require_operator().await,
        Err(Error::Forbidden)
    ));
    let (raw, raw_connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
    let raw_connection = tokio::spawn(raw_connection);
    let namespace = store.schema().namespace().to_owned();
    for sql in [
        format!("TRUNCATE \"{namespace}\".t0"),
        format!("UPDATE \"{namespace}\".metadata SET source=gen_random_uuid()"),
        format!("UPDATE \"{namespace}\".receipts SET authorization_scope=gen_random_uuid()"),
        format!("CREATE TABLE \"{namespace}\".escape(id integer)"),
        format!("UPDATE \"{namespace}\".scopes SET id=gen_random_uuid()"),
        format!("DELETE FROM \"{namespace}\".outbox"),
        format!("UPDATE \"{namespace}\".outbox SET attempts=0"),
    ] {
        assert!(
            raw.batch_execute(&sql).await.is_err(),
            "runtime unexpectedly allowed privileged SQL"
        );
    }
    raw.batch_execute(&format!("GRANT SELECT ON \"{namespace}\".t0 TO PUBLIC"))
        .await
        .unwrap();
    let public_access:bool=client.query_one("SELECT EXISTS(SELECT 1 FROM pg_class c,LATERAL aclexplode(c.relacl) a WHERE c.oid=to_regclass($1) AND a.grantee=0)",&[&format!("\"{namespace}\".t0")]).await.unwrap().get(0);
    assert!(!public_access);
    assert!(matches!(
        runtime.scope_status(context.scope).await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        runtime
            .compact_receipts(
                context.scope,
                Uuid::now_v7(),
                std::time::Duration::from_secs(1),
                1
            )
            .await,
        Err(Error::Forbidden)
    ));
    drop(raw);
    raw_connection.abort();
    drop(runtime);
    let worker_login = format!("worker_login_{suffix}");
    client.batch_execute(&format!("CREATE ROLE \"{worker_login}\" LOGIN PASSWORD '{password}' NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS")).await.unwrap();
    store
        .grant_role(&roles.worker, &worker_login)
        .await
        .unwrap();
    config.user(&worker_login);
    let mut worker = ManagedStore::connect_config(config.clone(), store.schema().clone())
        .await
        .unwrap();
    worker.verify().await.unwrap();
    assert!(
        worker
            .mutate(
                &registry,
                &context,
                Uuid::now_v7(),
                "insert",
                json!("denied")
            )
            .await
            .is_err()
    );
    assert!(matches!(
        worker.require_operator().await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        worker.scope_status(context.scope).await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        worker
            .compact_receipts(
                context.scope,
                Uuid::now_v7(),
                std::time::Duration::from_secs(1),
                1
            )
            .await,
        Err(Error::Forbidden)
    ));
    let source = store.managed_head(&context).await.unwrap().0;
    let jobs = worker
        .claim_outbox(source, hulladb_postgres::managed::ClaimOptions::default())
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].name, "notes.created");
    assert!(matches!(
        worker.requeue_outbox(jobs[0].id, source, source).await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        worker.discard_outbox(jobs[0].id, source, source).await,
        Err(Error::Forbidden)
    ));
    let (raw_worker, raw_worker_connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
    let raw_worker_connection = tokio::spawn(raw_worker_connection);
    for sql in [
        format!("SELECT * FROM \"{namespace}\".t0"),
        format!("UPDATE \"{namespace}\".outbox SET payload='null'::jsonb"),
        format!("UPDATE \"{namespace}\".outbox SET source=gen_random_uuid()"),
    ] {
        assert!(
            raw_worker.batch_execute(&sql).await.is_err(),
            "worker must not alter job meaning or access application data"
        );
    }
    drop(raw_worker);
    raw_worker_connection.abort();
    worker.acknowledge_outbox(&jobs[0]).await.unwrap();
    drop(worker);
    client
        .batch_execute(&format!("SET ROLE \"{}\"", roles.backup))
        .await
        .unwrap();
    assert_eq!(
        client
            .query_one(&format!("SELECT count(*) FROM \"{namespace}\".t0"), &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert!(
        client
            .batch_execute(&format!("DELETE FROM \"{namespace}\".t0"))
            .await
            .is_err()
    );
    client.batch_execute("RESET ROLE").await.unwrap();
    cleanup(&store).await;
    drop(store);
    client
        .batch_execute(&format!(
            "DROP ROLE \"{login}\";DROP ROLE \"{worker_login}\";DROP ROLE \"{}\";DROP ROLE \"{}\";DROP ROLE \"{}\";DROP ROLE \"{}\";",
            roles.backup, roles.runtime, roles.worker, roles.owner
        ))
        .await
        .unwrap();
    connection.abort();
}
#[tokio::test]
#[ignore = "requires PostgreSQL; run make production-test"]
async fn rotation_preserves_data_and_original_receipts_but_rejects_old_lifecycles() {
    let (mut store, registry, context) = fixture().await;
    let request = Uuid::now_v7();
    let receipt = store
        .mutate(&registry, &context, request, "insert", json!("old"))
        .await
        .unwrap();
    let source = store.rotate_source(receipt.source).await.unwrap();
    assert_ne!(source, receipt.source);
    assert_eq!(store.scope_usage(context.scope).await.unwrap().rows, 1);
    assert!(matches!(
        store
            .receipt_from_source(&context, request, Some(receipt.source))
            .await,
        Err(Error::SourceMismatch)
    ));
    assert!(matches!(
        store
            .receipt_from_source(&context, request, Some(source))
            .await,
        Err(Error::SourceMismatch)
    ));
    assert!(matches!(
        store
            .mutate_from_source(
                &registry,
                &context,
                Uuid::now_v7(),
                Some(receipt.source),
                "insert",
                json!("stale")
            )
            .await,
        Err(Error::SourceMismatch)
    ));
    let fresh = store
        .mutate_from_source(
            &registry,
            &context,
            Uuid::now_v7(),
            Some(source),
            "insert",
            json!("new"),
        )
        .await
        .unwrap();
    assert_eq!(fresh.revision.sequence, 2);
    cleanup(&store).await;
}
#[tokio::test]
async fn remote_database_configuration_cannot_silently_disable_tls() {
    let schema = ManagedSchema::new(
        "unused",
        SchemaDeclaration::new(vec![]).normalize().unwrap(),
    )
    .unwrap();
    assert!(matches!(
        ManagedStore::connect("postgresql://user:password@remote.example/db", schema).await,
        Err(Error::InvalidInput(_))
    ));
}

#[tokio::test]
#[ignore = "requires HULLADB_TLS_TEST_URL pointing to a trusted TLS/SCRAM PostgreSQL endpoint"]
async fn database_tls_authenticates_when_channel_binding_is_required() {
    let mut config: tokio_postgres::Config = std::env::var("HULLADB_TLS_TEST_URL")
        .expect("set HULLADB_TLS_TEST_URL to a disposable TLS/SCRAM PostgreSQL endpoint")
        .parse()
        .unwrap();
    config
        .ssl_mode(tokio_postgres::config::SslMode::Require)
        .channel_binding(tokio_postgres::config::ChannelBinding::Require);
    let schema = ManagedSchema::new(
        "unused",
        SchemaDeclaration::new(vec![]).normalize().unwrap(),
    )
    .unwrap();
    // Connecting performs authentication and session setup without creating any
    // application storage. The old adapter cannot supply the required binding.
    assert!(ManagedStore::connect_config(config, schema).await.is_ok());
}

#[tokio::test]
async fn database_tls_rejects_an_untrusted_certificate_before_authentication() {
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_rustls::{
        TlsAcceptor,
        rustls::{self, pki_types::PrivatePkcs8KeyDer},
    };
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![certificate.cert.der().clone()],
        PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der()).into(),
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let peer = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut startup = [0; 8];
        stream.read_exact(&mut startup).await.unwrap();
        assert_eq!(startup, [0, 0, 0, 8, 4, 210, 22, 47]);
        stream.write_all(b"S").await.unwrap();
        assert!(
            TlsAcceptor::from(Arc::new(config))
                .accept(stream)
                .await
                .is_err()
        );
    });
    let schema = ManagedSchema::new(
        "unused",
        SchemaDeclaration::new(vec![]).normalize().unwrap(),
    )
    .unwrap();
    let result = ManagedStore::connect(
        &format!("postgresql://user:secret@127.0.0.1:{port}/db?sslmode=require"),
        schema,
    )
    .await;
    assert!(matches!(result, Err(Error::Database(_))));
    peer.await.unwrap();
}

#[tokio::test]
async fn an_unresponsive_database_startup_has_a_bounded_deadline() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let peer = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    let schema = ManagedSchema::new(
        "unused",
        SchemaDeclaration::new(vec![]).normalize().unwrap(),
    )
    .unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        ManagedStore::connect(
            &format!("postgresql://user:secret@127.0.0.1:{port}/db?sslmode=disable"),
            schema,
        ),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(Error::ConnectionDeadline)));
    peer.abort();
}

// At risk: cleanup turning a committed request into a missing receipt would
// execute the mutation (and enqueue its external effects) a second time.
#[tokio::test]
#[ignore = "requires PostgreSQL; run make production-test"]
async fn receipt_compaction_is_bounded_fenced_and_never_reexecutes_committed_work() {
    use std::time::Duration;
    let (mut store, registry, context) = fixture().await;
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let relation = format!("\"{}\".receipts", store.schema().namespace());
    let input = json!(
        (0..20)
            .map(|_| Uuid::now_v7().to_string())
            .collect::<String>()
    );
    let requests = [Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()];
    let mut receipts = Vec::new();
    for request in requests {
        receipts.push(
            store
                .mutate(&registry, &context, request, "insert", input.clone())
                .await
                .unwrap(),
        );
    }
    let source = receipts[0].source;
    client.execute(&format!("UPDATE {relation} SET created_at=clock_timestamp()-interval '2 days' WHERE request_id<>$1"), &[&requests[2]]).await.unwrap();
    let before = store.scope_status(context.scope).await.unwrap();
    assert_eq!(before.receipts.retained, 3);
    assert_eq!(before.receipts.expired, 0);
    assert!(before.receipts.oldest_retained_age_ms.unwrap() >= 86_400_000);
    let age = Duration::from_secs(86_400);
    assert!(matches!(
        store
            .compact_receipts(context.scope, Uuid::now_v7(), age, 1)
            .await,
        Err(Error::SourceMismatch)
    ));
    assert!(matches!(
        store
            .compact_receipts(context.scope, source, Duration::ZERO, 1)
            .await,
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        store
            .compact_receipts(context.scope, source, age, 1001)
            .await,
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(
        store
            .compact_receipts(context.scope, source, age, 1)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .compact_receipts(context.scope, source, age, 1)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .compact_receipts(context.scope, source, age, 1)
            .await
            .unwrap(),
        0
    );
    let after = store.scope_status(context.scope).await.unwrap();
    assert_eq!(after.receipts.retained, 1);
    assert_eq!(after.receipts.expired, 2);
    assert!(after.receipts.oldest_retained_age_ms.unwrap() < 86_400_000);
    assert!(after.usage.receipt_bytes < before.usage.receipt_bytes);
    assert_eq!(after.usage.rows, 3);
    assert_eq!(after.usage.outbox_jobs, 3);
    assert_eq!(after.sequence, before.sequence);
    assert_eq!(client.query_one(&format!("SELECT count(*) FROM {relation} WHERE expired AND octet_length(input_digest)=32 AND receipt->'result'='null'::jsonb"), &[]).await.unwrap().get::<_, i64>(0), 2);
    for request in &requests[..2] {
        assert!(matches!(
            store.receipt(&context, *request).await,
            Err(Error::ReceiptExpired)
        ));
        for retry_input in [input.clone(), json!("changed arguments")] {
            assert!(matches!(
                store
                    .mutate(&registry, &context, *request, "insert", retry_input)
                    .await,
                Err(Error::ReceiptExpired)
            ));
        }
    }
    assert_eq!(
        store.receipt(&context, requests[2]).await.unwrap(),
        Some(receipts[2].clone())
    );
    let changed_claims = Context {
        authorization_scope: Uuid::now_v7(),
        ..context.clone()
    };
    assert!(matches!(
        store.receipt(&changed_claims, requests[0]).await,
        Err(Error::Forbidden)
    ));
    let other = Context {
        principal: Uuid::now_v7(),
        ..context.clone()
    };
    assert!(store.receipt(&other, requests[0]).await.unwrap().is_none());
    // Concurrent retention and duplicate execution serialize on the scope and
    // cannot manufacture a second row/job, whichever transaction wins.
    let mut compactor = ManagedStore::connect(&url(), store.schema().clone())
        .await
        .unwrap();
    compactor.verify().await.unwrap();
    client.execute(&format!("UPDATE {relation} SET created_at=clock_timestamp()-interval '2 days' WHERE request_id=$1"), &[&requests[2]]).await.unwrap();
    let (compacted, duplicate) = tokio::join!(
        compactor.compact_receipts(context.scope, source, age, 1),
        store.mutate(&registry, &context, requests[2], "insert", input.clone())
    );
    assert_eq!(compacted.unwrap(), 1);
    assert!(matches!(duplicate, Ok(_) | Err(Error::ReceiptExpired)));
    assert_eq!(store.scope_usage(context.scope).await.unwrap().rows, 3);
    let status = store.scope_status(context.scope).await.unwrap();
    assert_eq!(status.receipts.oldest_retained_age_ms, None);
    assert_eq!(status.usage.outbox_jobs, 3);
    let new_source = store.rotate_source(source).await.unwrap();
    assert!(matches!(
        store
            .receipt_from_source(&context, requests[0], Some(new_source))
            .await,
        Err(Error::SourceMismatch)
    ));
    // Existing development storage must be rejected rather than silently patched.
    client
        .batch_execute(&format!("ALTER TABLE {relation} DROP COLUMN expired"))
        .await
        .unwrap();
    let mut incompatible = ManagedStore::connect(&url(), store.schema().clone())
        .await
        .unwrap();
    assert!(matches!(
        incompatible.verify().await,
        Err(Error::SchemaMismatch)
    ));
    assert!(matches!(
        incompatible.initialize().await,
        Err(Error::SchemaMismatch)
    ));
    assert_eq!(
        client
            .query_one(&format!("SELECT count(*) FROM {relation}"), &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        3
    );
    cleanup(&store).await;
    connection.abort();
}

// At risk: dead/leased/old-source jobs counted as deliverable hide blocked work.
#[tokio::test]
#[ignore = "requires PostgreSQL; run make production-test"]
async fn operator_status_distinguishes_queue_states_and_quota_usage_without_payloads() {
    use hulladb_postgres::managed::{ClaimOptions, DeliveryError};
    use std::time::Duration;
    let (mut store, registry, context) = fixture().await;
    let empty = store.scope_status(context.scope).await.unwrap();
    assert_eq!(empty.receipts.retained, 0);
    assert_eq!(empty.receipts.oldest_retained_age_ms, None);
    assert_eq!(empty.outbox.oldest_ready_age_ms, None);
    for _ in 0..5 {
        store
            .mutate(
                &registry,
                &context,
                Uuid::now_v7(),
                "insert",
                json!("private payload"),
            )
            .await
            .unwrap();
    }
    let source = store.managed_head(&context).await.unwrap().0;
    let claim = ClaimOptions {
        limit: 1,
        lease: Duration::from_secs(60),
    };
    let dead = store
        .claim_outbox(source, claim)
        .await
        .unwrap()
        .pop()
        .unwrap();
    store
        .retry_outbox(
            &dead,
            Duration::ZERO,
            DeliveryError::Permanent("private reason".into()),
        )
        .await
        .unwrap();
    let leased = store
        .claim_outbox(source, claim)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let delayed = store
        .claim_outbox(source, claim)
        .await
        .unwrap()
        .pop()
        .unwrap();
    store
        .retry_outbox(
            &delayed,
            Duration::from_secs(3600),
            DeliveryError::Retry("private reason".into()),
        )
        .await
        .unwrap();
    let quota = ScopeQuota {
        max_outbox_jobs: Some(10),
        ..Default::default()
    };
    store
        .set_scope_quota(context.scope, quota.clone())
        .await
        .unwrap();
    let status = store.scope_status(context.scope).await.unwrap();
    assert_eq!(status.quota, quota);
    assert_eq!(status.outbox.ready, 2);
    assert_eq!(status.outbox.delayed, 1);
    assert_eq!(status.outbox.leased, 1);
    assert_eq!(status.outbox.dead, 1);
    assert_eq!(status.outbox.old_source, 0);
    assert!(status.outbox.oldest_ready_age_ms.is_some());
    assert!(!serde_json::to_string(&status).unwrap().contains("private"));
    store.acknowledge_outbox(&leased).await.unwrap();
    let new_source = store.rotate_source(source).await.unwrap();
    store
        .mutate(
            &registry,
            &context,
            Uuid::now_v7(),
            "insert",
            json!("fresh"),
        )
        .await
        .unwrap();
    let status = store.scope_status(context.scope).await.unwrap();
    assert_eq!(status.source, new_source);
    assert_eq!(status.outbox.old_source, 4);
    assert_eq!(status.outbox.ready, 1);
    assert_eq!(
        status.outbox.dead + status.outbox.leased + status.outbox.delayed,
        0
    );
    assert_eq!(status.usage.outbox_jobs, 5);
    assert!(matches!(
        store.scope_status(Uuid::now_v7()).await,
        Err(Error::NotFound)
    ));
    cleanup(&store).await;
}

// Production admission protects first-delivery binding, durable failures and
// response loss using the same runtime transactions as business mutations.
#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn reusable_admission_replays_success_and_terminal_failure() {
    let (mut store, registry, context) = fixture().await;
    let ticket = store.issue_ticket(&context).await.unwrap();
    let (source, _) = store.managed_head(&context).await.unwrap();
    let request = Uuid::new_v4();
    let receipt = store
        .mutate_with_ticket(
            &registry,
            &context,
            ticket.id,
            request,
            source,
            "insert",
            json!("original"),
        )
        .await
        .unwrap();
    // Recover from a fresh connection: no application callback executes.
    let mut recovered = ManagedStore::connect(&url(), store.schema().clone())
        .await
        .unwrap();
    recovered.verify().await.unwrap();
    assert_eq!(
        recovered
            .receipt_with_ticket(&context, ticket.id, request, source)
            .await
            .unwrap(),
        Some(receipt.clone())
    );
    assert_eq!(
        recovered
            .mutate_with_ticket(
                &registry,
                &context,
                ticket.id,
                request,
                source,
                "insert",
                json!("original")
            )
            .await
            .unwrap(),
        receipt
    );
    assert!(matches!(
        recovered
            .mutate_with_ticket(
                &registry,
                &context,
                ticket.id,
                request,
                source,
                "insert",
                json!("different")
            )
            .await,
        Err(Error::RequestIdReused)
    ));
    let renewed = store.issue_ticket(&context).await.unwrap();
    assert!(matches!(
        recovered
            .receipt_with_ticket(&context, renewed.id, request, source)
            .await,
        Err(Error::RequestIdReused)
    ));
    let failed = Uuid::new_v4();
    assert!(
        matches!(store.mutate_with_ticket(&registry,&context,ticket.id,failed,source,"insert",json!(false)).await,Err(Error::TerminalFailure(ref code)) if code=="invalid_request")
    );
    assert!(
        matches!(recovered.receipt_with_ticket(&context,ticket.id,failed,source).await,Err(Error::TerminalFailure(ref code)) if code=="invalid_request")
    );
    assert!(
        matches!(recovered.mutate_with_ticket(&registry,&context,ticket.id,failed,source,"insert",json!(false)).await,Err(Error::TerminalFailure(ref code)) if code=="invalid_request")
    );
    recovered
        .acknowledge_ticket(&context, ticket.id, request, source)
        .await
        .unwrap();
    assert!(matches!(
        store
            .mutate_with_ticket(
                &registry,
                &context,
                ticket.id,
                request,
                source,
                "insert",
                json!("original")
            )
            .await,
        Err(Error::ReceiptExpired)
    ));
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn concurrent_ticket_deliveries_commit_one_business_effect() {
    let (mut store, registry, context) = fixture().await;
    let mut duplicate = ManagedStore::connect(&url(), store.schema().clone())
        .await
        .unwrap();
    duplicate.verify().await.unwrap();
    let ticket = store.issue_ticket(&context).await.unwrap();
    let (source, _) = store.managed_head(&context).await.unwrap();
    let request = Uuid::new_v4();
    let (first, second) = tokio::join!(
        store.mutate_with_ticket(
            &registry,
            &context,
            ticket.id,
            request,
            source,
            "insert",
            json!("once")
        ),
        duplicate.mutate_with_ticket(
            &registry,
            &context,
            ticket.id,
            request,
            source,
            "insert",
            json!("once")
        )
    );
    assert_eq!(first.unwrap(), second.unwrap());
    let status = store.scope_status(context.scope).await.unwrap();
    assert_eq!(status.usage.rows, 1);
    assert_eq!(status.usage.outbox_jobs, 1);
    let conflicting = Uuid::new_v4();
    let (left, right) = tokio::join!(
        store.mutate_with_ticket(
            &registry,
            &context,
            ticket.id,
            conflicting,
            source,
            "insert",
            json!("left")
        ),
        duplicate.mutate_with_ticket(
            &registry,
            &context,
            ticket.id,
            conflicting,
            source,
            "insert",
            json!("right")
        ),
    );
    let winner = match (left, right) {
        (Ok(receipt), Err(Error::RequestIdReused)) | (Err(Error::RequestIdReused), Ok(receipt)) => {
            receipt
        }
        other => panic!("conflicting first deliveries must bind one winner: {other:?}"),
    };
    assert_eq!(
        store
            .receipt_with_ticket(&context, ticket.id, conflicting, source)
            .await
            .unwrap(),
        Some(winner)
    );
    assert_eq!(
        store.scope_status(context.scope).await.unwrap().usage.rows,
        2
    );
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn ticket_expiry_preserves_confirmation_then_purge_prevents_reentry() {
    let (mut store, registry, context) = fixture().await;
    let ticket = store.issue_ticket(&context).await.unwrap();
    let (source, _) = store.managed_head(&context).await.unwrap();
    let request = Uuid::new_v4();
    let receipt = store
        .mutate_with_ticket(
            &registry,
            &context,
            ticket.id,
            request,
            source,
            "insert",
            json!("retained"),
        )
        .await
        .unwrap();
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let namespace = store.schema().namespace().to_owned();
    client.execute(&format!("UPDATE \"{namespace}\".tickets SET expires_at=clock_timestamp()-interval '1 second' WHERE id=$1"),&[&ticket.id]).await.unwrap();
    assert!(matches!(
        store
            .mutate_with_ticket(
                &registry,
                &context,
                ticket.id,
                Uuid::new_v4(),
                source,
                "insert",
                json!("late")
            )
            .await,
        Err(Error::InvalidTicket)
    ));
    assert_eq!(
        store.cleanup_admissions(100).await.unwrap().retired_tickets,
        1
    );
    assert_eq!(
        store
            .receipt_with_ticket(&context, ticket.id, request, source)
            .await
            .unwrap(),
        Some(receipt)
    );
    let changed = Context {
        authorization_scope: Uuid::new_v4(),
        ..context.clone()
    };
    assert!(matches!(
        store
            .receipt_with_ticket(&changed, ticket.id, request, source)
            .await,
        Err(Error::Forbidden)
    ));
    client.execute(&format!("UPDATE \"{namespace}\".admissions SET result_until=clock_timestamp()-interval '1 second',purge_after=clock_timestamp()-interval '1 second' WHERE request_id=$1"),&[&request]).await.unwrap();
    assert_eq!(
        store.cleanup_admissions(100).await.unwrap().purged_requests,
        1
    );
    assert!(
        store
            .receipt_with_ticket(&context, ticket.id, request, source)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        store
            .mutate_with_ticket(
                &registry,
                &context,
                ticket.id,
                request,
                source,
                "insert",
                json!("retained")
            )
            .await,
        Err(Error::InvalidTicket)
    ));
    connection.abort();
    cleanup(&store).await;
}

struct PausedInsert(
    std::sync::Arc<tokio::sync::Notify>,
    Option<std::sync::Arc<tokio::sync::Notify>>,
);
impl FunctionContract for PausedInsert {
    fn validate_input(&self, _: &Json) -> Result<(), Error> {
        Ok(())
    }
    fn validate_result(&self, _: &Json) -> Result<(), Error> {
        Ok(())
    }
}
impl Mutation<()> for PausedInsert {
    fn call<'a>(
        &'a self,
        context: MutationContext<'a, ()>,
        _input: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        Box::pin(async move {
            context
                .db
                .insert(
                    "notes",
                    RecordValues::from([("text".into(), Value::String("rolled back".into()))]),
                )
                .await?;
            self.0.notify_one();
            if let Some(release) = &self.1 {
                release.notified().await;
                Ok(json!(null))
            } else {
                std::future::pending().await
            }
        })
    }
}
#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn cancelled_execution_rolls_back_before_cleanup_fences_failure() {
    let (mut store, _, context) = fixture().await;
    let schema = store.schema().clone();
    let ticket = store.issue_ticket(&context).await.unwrap();
    let (source, _) = store.managed_head(&context).await.unwrap();
    let request = Uuid::new_v4();
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let mut registry = Registry::new();
    registry
        .mutation("paused", PausedInsert(entered.clone(), None), Permit)
        .unwrap();
    let owned_context = context.clone();
    let execution = tokio::spawn(async move {
        store
            .mutate_with_ticket(
                &registry,
                &owned_context,
                ticket.id,
                request,
                source,
                "paused",
                json!(null),
            )
            .await
    });
    entered.notified().await;
    let mut recovery = ManagedStore::connect(&url(), schema).await.unwrap();
    recovery.verify().await.unwrap();
    assert!(
        recovery
            .receipt_with_ticket(&context, ticket.id, request, source)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        recovery
            .cleanup_admissions(100)
            .await
            .unwrap()
            .failed_requests,
        0
    );
    execution.abort();
    assert!(execution.await.unwrap_err().is_cancelled());
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    // UPDATE waits for PostgreSQL's row lock, proving the cancelled business
    // transaction has rolled back before advancing this fixture's deadline.
    client.execute(&format!("UPDATE \"{}\".admissions SET work_until=clock_timestamp()-interval '1 second' WHERE request_id=$1",recovery.schema().namespace()),&[&request]).await.unwrap();
    assert_eq!(
        recovery
            .cleanup_admissions(100)
            .await
            .unwrap()
            .failed_requests,
        1
    );
    assert!(
        matches!(recovery.receipt_with_ticket(&context,ticket.id,request,source).await,Err(Error::TerminalFailure(ref code))if code=="execution_limit")
    );
    assert_eq!(
        recovery
            .scope_status(context.scope)
            .await
            .unwrap()
            .usage
            .rows,
        0
    );
    connection.abort();
    cleanup(&recovery).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn admission_capacity_is_transient_but_business_quota_failure_is_durable() {
    let (mut store, registry, context) = fixture().await;
    store
        .set_scope_quota(
            context.scope,
            ScopeQuota {
                max_receipt_bytes: Some(0),
                ..ScopeQuota::default()
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        store.issue_ticket(&context).await,
        Err(Error::QuotaExceeded)
    ));
    store
        .set_scope_quota(context.scope, ScopeQuota::default())
        .await
        .unwrap();
    let ticket = store.issue_ticket(&context).await.unwrap();
    let (source, _) = store.managed_head(&context).await.unwrap();
    let used = store
        .scope_status(context.scope)
        .await
        .unwrap()
        .usage
        .receipt_bytes;
    store
        .set_scope_quota(
            context.scope,
            ScopeQuota {
                max_receipt_bytes: Some(used),
                ..ScopeQuota::default()
            },
        )
        .await
        .unwrap();
    let request = Uuid::new_v4();
    assert!(matches!(
        store
            .mutate_with_ticket(
                &registry,
                &context,
                ticket.id,
                request,
                source,
                "insert",
                json!("later")
            )
            .await,
        Err(Error::QuotaExceeded)
    ));
    assert!(
        store
            .receipt_with_ticket(&context, ticket.id, request, source)
            .await
            .unwrap()
            .is_none()
    );
    store
        .set_scope_quota(context.scope, ScopeQuota::default())
        .await
        .unwrap();
    // An ACK sent before first delivery cannot create or retire an identity.
    store
        .acknowledge_ticket(&context, ticket.id, request, source)
        .await
        .unwrap();
    let receipt = store
        .mutate_with_ticket(
            &registry,
            &context,
            ticket.id,
            request,
            source,
            "insert",
            json!("later"),
        )
        .await
        .unwrap();
    store
        .set_scope_quota(
            context.scope,
            ScopeQuota {
                max_result_bytes: Some(0),
                ..ScopeQuota::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .mutate_with_ticket(
                &registry,
                &context,
                ticket.id,
                request,
                source,
                "insert",
                json!("later")
            )
            .await
            .unwrap(),
        receipt
    );
    let failed = Uuid::new_v4();
    assert!(
        matches!(store.mutate_with_ticket(&registry,&context,ticket.id,failed,source,"insert",json!("rollback")).await,Err(Error::TerminalFailure(ref code))if code=="quota_exceeded")
    );
    store
        .set_scope_quota(context.scope, ScopeQuota::default())
        .await
        .unwrap();
    assert!(
        matches!(store.receipt_with_ticket(&context,ticket.id,failed,source).await,Err(Error::TerminalFailure(ref code))if code=="quota_exceeded")
    );
    assert!(matches!(
        store
            .mutate(&registry, &context, failed, "insert", json!("rollback"))
            .await,
        Err(Error::RequestIdReused)
    ));
    assert_eq!(
        store.scope_status(context.scope).await.unwrap().usage.rows,
        1
    );
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn active_execution_keeps_frozen_confirmation_after_accepted_deadline() {
    let (mut store, _, context) = fixture().await;
    let schema = store.schema().clone();
    let ticket = store.issue_ticket(&context).await.unwrap();
    let (source, _) = store.managed_head(&context).await.unwrap();
    let request = Uuid::new_v4();
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let digest = ring::digest::digest(&ring::digest::SHA256, b"null")
        .as_ref()
        .to_vec();
    // Seed an already accepted request with a short frozen execution window.
    // Production execution must honor this row rather than refresh its deadlines.
    client.execute(&format!("INSERT INTO \"{}\".admissions(scope,principal,request_id,ticket,authorization_scope,source,operation,input_digest,work_until,purge_after) VALUES($1,$2,$3,$4,$5,$6,'paused',$7,clock_timestamp()+interval '2 seconds',clock_timestamp()+interval '30 seconds')",schema.namespace()),&[&context.scope,&context.principal,&request,&ticket.id,&context.authorization_scope,&source,&digest]).await.unwrap();
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    let mut registry = Registry::new();
    registry
        .mutation(
            "paused",
            PausedInsert(entered.clone(), Some(release.clone())),
            Permit,
        )
        .unwrap();
    let owned_context = context.clone();
    let execution = tokio::spawn(async move {
        let result = store
            .mutate_with_ticket(
                &registry,
                &owned_context,
                ticket.id,
                request,
                source,
                "paused",
                json!(null),
            )
            .await;
        (store, result)
    });
    entered.notified().await;
    let mut maintenance = ManagedStore::connect(&url(), schema).await.unwrap();
    maintenance.verify().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    assert_eq!(
        maintenance
            .cleanup_admissions(100)
            .await
            .unwrap()
            .failed_requests,
        0
    );
    assert!(
        maintenance
            .receipt_with_ticket(&context, ticket.id, request, source)
            .await
            .unwrap()
            .is_none()
    );
    release.notify_one();
    let (store, result) = execution.await.unwrap();
    let receipt = result.unwrap();
    assert_eq!(
        maintenance
            .receipt_with_ticket(&context, ticket.id, request, source)
            .await
            .unwrap(),
        Some(receipt)
    );
    assert_eq!(
        maintenance
            .scope_status(context.scope)
            .await
            .unwrap()
            .usage
            .rows,
        1
    );
    connection.abort();
    cleanup(&store).await;
}

struct PausedFailure(PausedInsert, Option<hulladb_protocol::ApplicationError>);
impl FunctionContract for PausedFailure {
    fn validate_input(&self, _: &Json) -> Result<(), Error> {
        Ok(())
    }
    fn validate_result(&self, _: &Json) -> Result<(), Error> {
        Ok(())
    }
}
impl Mutation<()> for PausedFailure {
    fn call<'a>(
        &'a self,
        context: MutationContext<'a, ()>,
        input: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        Box::pin(async move {
            self.0.call(context, input).await?;
            Err(self
                .1
                .clone()
                .map(Error::Application)
                .unwrap_or(Error::Conflict))
        })
    }
}
#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn duplicate_winning_after_rollback_is_returned_by_failure_finalizer() {
    let (mut store, _, context) = fixture().await;
    let schema = store.schema().clone();
    let ticket = store.issue_ticket(&context).await.unwrap();
    let (source, _) = store.managed_head(&context).await.unwrap();
    let request = Uuid::new_v4();
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    let mut original_registry = Registry::new();
    original_registry
        .mutation(
            "insert",
            PausedFailure(PausedInsert(entered.clone(), Some(release.clone())), None),
            Permit,
        )
        .unwrap();
    let original_context = context.clone();
    let original = tokio::spawn(async move {
        let result = store
            .mutate_with_ticket(
                &original_registry,
                &original_context,
                ticket.id,
                request,
                source,
                "insert",
                json!("winner"),
            )
            .await;
        (store, result)
    });
    entered.notified().await;
    let app_name = format!("admission_duplicate_{}", Uuid::new_v4().simple());
    let mut config: tokio_postgres::Config = url().parse().unwrap();
    config.application_name(&app_name);
    let mut duplicate = ManagedStore::connect_config(config, schema).await.unwrap();
    duplicate.verify().await.unwrap();
    let mut registry = Registry::new();
    registry.mutation("insert", Insert, Permit).unwrap();
    let duplicate_context = context.clone();
    let duplicate = tokio::spawn(async move {
        duplicate
            .mutate_with_ticket(
                &registry,
                &duplicate_context,
                ticket.id,
                request,
                source,
                "insert",
                json!("winner"),
            )
            .await
    });
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    // Observe the duplicate queued on the original scope lock before rollback.
    tokio::time::timeout(std::time::Duration::from_secs(2),async{loop{
        let waiting:bool=client.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name=$1 AND wait_event_type='Lock')",&[&app_name]).await.unwrap().get(0);
        if waiting {break} tokio::task::yield_now().await;
    }}).await.unwrap();
    release.notify_one();
    let (mut store, first) = original.await.unwrap();
    let second = duplicate.await.unwrap().unwrap();
    assert_eq!(first.unwrap(), second);
    assert_eq!(
        store.scope_status(context.scope).await.unwrap().usage.rows,
        1
    );
    connection.abort();
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn existing_native_receipt_cannot_be_rebound_to_a_new_ticket() {
    let (mut store, registry, context) = fixture().await;
    let request = Uuid::new_v4();
    let receipt = store
        .mutate(&registry, &context, request, "insert", json!("native"))
        .await
        .unwrap();
    let ticket = store.issue_ticket(&context).await.unwrap();
    assert!(matches!(
        store
            .mutate_with_ticket(
                &registry,
                &context,
                ticket.id,
                request,
                receipt.source,
                "insert",
                json!("native")
            )
            .await,
        Err(Error::RequestIdReused)
    ));
    assert!(
        store
            .receipt_with_ticket(&context, ticket.id, request, receipt.source)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.receipt(&context, request).await.unwrap(),
        Some(receipt)
    );
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn admission_result_cleanup_is_bounded_and_does_not_revisit_compacted_rows() {
    let (mut store, registry, context) = fixture().await;
    let ticket = store.issue_ticket(&context).await.unwrap();
    let (source, _) = store.managed_head(&context).await.unwrap();
    let mut requests = Vec::new();
    for _ in 0..3 {
        let request = Uuid::new_v4();
        store
            .mutate_with_ticket(
                &registry,
                &context,
                ticket.id,
                request,
                source,
                "insert",
                json!("retained"),
            )
            .await
            .unwrap();
        requests.push(request);
    }
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!(
            "UPDATE \"{}\".admissions SET result_until=clock_timestamp()-interval '1 second'",
            store.schema().namespace()
        ))
        .await
        .unwrap();
    for _ in 0..3 {
        let batch = store.cleanup_admissions(1).await.unwrap();
        assert_eq!(batch.compacted_results, 1);
        assert_eq!(batch.purged_requests, 0);
    }
    assert_eq!(
        store.cleanup_admissions(1).await.unwrap().compacted_results,
        0
    );
    for request in requests {
        assert!(matches!(
            store
                .receipt_with_ticket(&context, ticket.id, request, source)
                .await,
            Err(Error::ReceiptExpired)
        ));
    }
    let compacted:i64=client.query_one(&format!("SELECT count(*) FROM \"{}\".receipts WHERE expired AND receipt->'result'='null'::jsonb",store.schema().namespace()),&[]).await.unwrap().get(0);
    assert_eq!(compacted, 3);
    connection.abort();
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn pending_quota_reservation_pays_for_failure_without_exceeding_limit() {
    let (mut store, _, context) = fixture().await;
    let schema = store.schema().clone();
    let ticket = store.issue_ticket(&context).await.unwrap();
    let (source, _) = store.managed_head(&context).await.unwrap();
    let request = Uuid::new_v4();
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    let mut registry = Registry::new();
    registry
        .mutation(
            "fail",
            PausedFailure(
                PausedInsert(entered.clone(), Some(release.clone())),
                Some(hulladb_protocol::ApplicationError {
                    code: "TASK_LOCKED".into(),
                    message: "x".repeat(16000),
                    details: None,
                }),
            ),
            Permit,
        )
        .unwrap();
    let owned = context.clone();
    let execution = tokio::spawn(async move {
        let result = store
            .mutate_with_ticket(
                &registry,
                &owned,
                ticket.id,
                request,
                source,
                "fail",
                json!(null),
            )
            .await;
        (store, result)
    });
    entered.notified().await;
    let mut observer = ManagedStore::connect(&url(), schema).await.unwrap();
    observer.verify().await.unwrap();
    let reserved = observer
        .scope_status(context.scope)
        .await
        .unwrap()
        .usage
        .receipt_bytes;
    // This operator change sets the existing reservation exactly at capacity.
    // It is deliberately done with SQL because a long-running business write
    // holds the scope key-share lock used by the ordinary quota administration.
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    client
        .execute(
            &format!(
                "INSERT INTO \"{}\".quotas(scope,max_receipt_bytes) VALUES($1,$2)",
                observer.schema().namespace()
            ),
            &[&context.scope, &reserved],
        )
        .await
        .unwrap();
    release.notify_one();
    let (store, result) = execution.await.unwrap();
    assert!(
        matches!(result,Err(Error::Application(ref error))if error.code=="TASK_LOCKED" && error.message.len()==16000)
    );
    let status = observer.scope_status(context.scope).await.unwrap();
    assert!(status.usage.receipt_bytes <= reserved);
    assert_eq!(status.usage.rows, 0);
    assert!(
        matches!(observer.receipt_with_ticket(&context,ticket.id,request,source).await,Err(Error::Application(ref error))if error.code=="TASK_LOCKED" && error.message.len()==16000)
    );
    connection.abort();
    cleanup(&store).await;
}

struct DelayedNative {
    entered: std::sync::Arc<tokio::sync::Notify>,
    release: std::sync::Arc<tokio::sync::Notify>,
}
impl FunctionContract for DelayedNative {
    fn validate_input(&self, _: &Json) -> Result<(), Error> {
        Ok(())
    }
    fn validate_result(&self, _: &Json) -> Result<(), Error> {
        Ok(())
    }
}
impl Mutation<()> for DelayedNative {
    fn call<'a>(
        &'a self,
        context: MutationContext<'a, ()>,
        _: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            context
                .db
                .insert(
                    "notes",
                    RecordValues::from([("text".into(), Value::String("must not commit".into()))]),
                )
                .await?;
            Ok(json!("written"))
        })
    }
}
#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn already_running_native_request_cannot_commit_after_ticket_failure() {
    let (mut native, registry, context) = fixture().await;
    let schema = native.schema().clone();
    let mut ticketed = ManagedStore::connect(&url(), schema).await.unwrap();
    ticketed.verify().await.unwrap();
    let ticket = ticketed.issue_ticket(&context).await.unwrap();
    let (source, _) = ticketed.managed_head(&context).await.unwrap();
    let request = Uuid::new_v4();
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    let mut delayed = Registry::new();
    delayed
        .mutation(
            "insert",
            DelayedNative {
                entered: entered.clone(),
                release: release.clone(),
            },
            Permit,
        )
        .unwrap();
    let owned = context.clone();
    let running = tokio::spawn(async move {
        native
            .mutate(&delayed, &owned, request, "insert", json!(false))
            .await
    });
    entered.notified().await;
    assert!(
        matches!(ticketed.mutate_with_ticket(&registry,&context,ticket.id,request,source,"insert",json!(false)).await,Err(Error::TerminalFailure(ref code))if code=="invalid_request")
    );
    release.notify_one();
    let outcome = running.await.unwrap();
    assert!(
        matches!(outcome, Err(Error::RequestIdReused)),
        "unticketed execution crossed a terminal ticket outcome: {outcome:?}"
    );
    assert_eq!(
        ticketed
            .scope_status(context.scope)
            .await
            .unwrap()
            .usage
            .rows,
        0
    );
    cleanup(&ticketed).await;
}
