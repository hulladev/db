#![cfg(feature = "server")]

use hulladb::{
    AdminDatabase, Committed, Context, Error, Id, ManagedSchema, ManagedStore, ScopeQuota, Table,
    Uuid,
};
#[derive(Table)]
#[table(name = "entries")]
pub struct Entry {
    pub id: Id<Entry>,
    pub title: String,
}
fn url() -> String {
    std::env::var("HULLADB_TEST_URL")
        .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into())
}
#[tokio::test]
#[ignore = "requires PostgreSQL; run make production-test"]
async fn typed_admin_transactions_preserve_constraints_quotas_revisions_and_receipts() {
    let schema = ManagedSchema::new(
        format!("admin_{}", Uuid::now_v7().simple()),
        hulladb::schema!(Entry).unwrap(),
    )
    .unwrap();
    let mut store = ManagedStore::connect(&url(), schema.clone()).await.unwrap();
    store.initialize().await.unwrap();
    let context = Context {
        scope: Uuid::now_v7(),
        principal: Uuid::now_v7(),
        authorization_scope: Uuid::now_v7(),
        identity: (),
    };
    store.create_scope(context.scope).await.unwrap();
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
    let mut admin = AdminDatabase::connect(&url(), schema.clone(), context.clone())
        .await
        .unwrap();
    let request = Uuid::now_v7();
    let rejected = admin
        .transaction("jobs.import", request, "two".to_owned(), |ctx, title| {
            Box::pin(async move {
                let one = ctx
                    .db
                    .insert(EntryInsert {
                        title: title.clone(),
                    })
                    .await?;
                let two = ctx.db.insert(EntryInsert { title }).await?;
                Ok(vec![one, two])
            })
        })
        .await;
    assert!(matches!(rejected, Err(Error::QuotaExceeded)));
    assert_eq!(store.scope_usage(context.scope).await.unwrap().rows, 0);
    assert_eq!(store.managed_head(&context).await.unwrap().1.sequence, 0);
    assert!(store.receipt(&context, request).await.unwrap().is_none());
    store
        .set_scope_quota(
            context.scope,
            ScopeQuota {
                max_rows: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let committed = admin
        .transaction("jobs.import", request, "two".to_owned(), |ctx, title| {
            Box::pin(async move {
                let one = ctx
                    .db
                    .insert(EntryInsert {
                        title: title.clone(),
                    })
                    .await?;
                let two = ctx.db.insert(EntryInsert { title }).await?;
                Ok(vec![one, two])
            })
        })
        .await
        .unwrap();
    assert_eq!(committed.receipt.revision.sequence, 1);
    let recovered: Committed<Vec<Entry>> = admin
        .transaction("jobs.import", request, "two".to_owned(), |_, _| {
            Box::pin(async { panic!("receipt recovery reran an admin handler") })
        })
        .await
        .unwrap();
    assert_eq!(recovered.data[0].id, committed.data[0].id);
    assert_eq!(recovered.receipt, committed.receipt);
    let rows: Vec<Entry> = admin
        .query("jobs.all", (), |ctx, _| {
            Box::pin(async move { ctx.db.select::<Entry>().take(10).await })
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    let notification_request = Uuid::now_v7();
    let notification: Committed<Uuid> = admin
        .transaction(
            "jobs.notify",
            notification_request,
            "queued".to_owned(),
            |ctx, payload| {
                Box::pin(async move { ctx.db.enqueue("entries.created", payload).await })
            },
        )
        .await
        .unwrap();
    let recovered: Committed<Uuid> = admin
        .transaction(
            "jobs.notify",
            notification_request,
            "queued".to_owned(),
            |_, _| {
                Box::pin(async { panic!("outbox receipt replay must not enqueue a second job") })
            },
        )
        .await
        .unwrap();
    assert_eq!(notification.data, recovered.data);
    assert_eq!(notification.receipt.revision, committed.receipt.revision);
    let jobs = store
        .claim_outbox(
            notification.receipt.source,
            hulladb::ClaimOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, notification.data);
    assert_eq!(jobs[0].payload, "queued");
    store.acknowledge_outbox(&jobs[0]).await.unwrap();
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!("DROP SCHEMA \"{}\" CASCADE", schema.namespace()))
        .await
        .unwrap();
    connection.abort();
}

#[tokio::test]
#[ignore = "requires PostgreSQL; run make production-test"]
async fn direct_transactions_run_once_and_only_replay_with_explicit_retries() {
    use hulladb::TransactionOptions;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    let schema = ManagedSchema::new(
        format!("admin_retry_{}", Uuid::now_v7().simple()),
        hulladb::schema!(Entry).unwrap(),
    )
    .unwrap();
    let mut store = ManagedStore::connect(&url(), schema.clone()).await.unwrap();
    store.initialize().await.unwrap();
    let context = Context {
        scope: Uuid::now_v7(),
        principal: Uuid::now_v7(),
        authorization_scope: Uuid::now_v7(),
        identity: (),
    };
    store.create_scope(context.scope).await.unwrap();
    let mut admin = AdminDatabase::connect(&url(), schema.clone(), context.clone())
        .await
        .unwrap();
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let namespace = schema.namespace();
    client
        .batch_execute(&format!(
            "CREATE SEQUENCE \"{namespace}\".fault_attempt; \
        CREATE FUNCTION \"{namespace}\".retry_commit() RETURNS trigger LANGUAGE plpgsql AS $$ \
        BEGIN IF nextval('\"{namespace}\".fault_attempt') <= 4 THEN \
        RAISE EXCEPTION 'serialization fault' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; \
        CREATE CONSTRAINT TRIGGER retry_commit AFTER INSERT ON \"{namespace}\".receipts \
        DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION \"{namespace}\".retry_commit();"
        ))
        .await
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let once: Result<Committed<Entry>, Error> = admin
        .transaction(
            "jobs.once",
            Uuid::now_v7(),
            "once".to_owned(),
            move |ctx, title| {
                counted.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move { ctx.db.insert(EntryInsert { title }).await })
            },
        )
        .await;
    assert!(matches!(once, Err(Error::RetryExhausted)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(store.scope_usage(context.scope).await.unwrap().rows, 0);
    assert_eq!(store.managed_head(&context).await.unwrap().1.sequence, 0);

    let counted = calls.clone();
    let committed: Committed<Entry> = admin
        .transaction_with_options(
            "jobs.retried",
            Uuid::now_v7(),
            "retried".to_owned(),
            move |ctx, title| {
                counted.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move { ctx.db.insert(EntryInsert { title }).await })
            },
            TransactionOptions {
                retries: 3,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 5);
    assert_eq!(committed.receipt.revision.sequence, 1);
    assert_eq!(store.scope_usage(context.scope).await.unwrap().rows, 1);

    let counted = calls.clone();
    let application_error: Result<Committed<Entry>, Error> = admin
        .transaction_with_options(
            "jobs.rejected",
            Uuid::now_v7(),
            "rejected".to_owned(),
            move |ctx, title| {
                counted.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    ctx.db.insert(EntryInsert { title }).await?;
                    Err(Error::InvalidInput("application rejected write".into()))
                })
            },
            TransactionOptions {
                retries: 3,
                ..Default::default()
            },
        )
        .await;
    assert!(matches!(application_error, Err(Error::InvalidInput(_))));
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    assert_eq!(store.scope_usage(context.scope).await.unwrap().rows, 1);
    assert_eq!(store.managed_head(&context).await.unwrap().1.sequence, 1);
    client
        .batch_execute(&format!("DROP SCHEMA \"{namespace}\" CASCADE"))
        .await
        .unwrap();
    connection.abort();
}
