#![cfg(feature = "server")]

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use hulladb::{
    Authorization, Codec, Context, Error, FunctionFuture, Id, Invocation, LocalRuntime,
    ManagedSchema, ManagedStore, MutationContext, QueryContext, QueryState, Registry, Table, Uuid,
    mutation, query,
};
use serde_json::{Value as Json, json};
#[path = "support/model.rs"]
mod model;
use model::*;

// Function attributes must preserve conditional compilation on every generated
// item, including signature aliases and the watch/registration module.
#[query]
#[cfg(any())]
async fn disabled(_: QueryContext<'_, AbsentIdentity>) -> Result<AbsentRow, Error> {
    unreachable!()
}

#[query(name = "articles.pending")]
async fn pending(ctx: QueryContext<'_, String>) -> Result<Vec<Article>, Error> {
    ctx.db
        .select::<Article>()
        .where_eq(Article::fields().publication, Id::new(ctx.identity.clone()))
        .where_eq(Article::fields().published, false)
        .take(50)
        .await
}

#[mutation(name = "articles.create")]
async fn create(ctx: MutationContext<'_, String>, input: CreateArticle) -> Result<Article, Error> {
    if input.publication.key() != ctx.identity {
        return Err(Error::Forbidden);
    }
    if input.title.trim().is_empty() {
        return Err(Error::InvalidInput("title is required".into()));
    }
    if ctx.db.get(input.publication.clone()).await?.is_none() {
        ctx.db
            .insert(PublicationInsert {
                slug: ctx.identity.clone(),
                title: "Daily".into(),
            })
            .await?;
    }
    ctx.db
        .insert(ArticleInsert {
            publication: input.publication,
            title: input.title,
            published: Some(input.published),
            ordinal: Some(input.ordinal),
            excerpt: Some(input.notes),
            origin: None,
        })
        .await
}

#[mutation(name = "articles.fail")]
async fn fail(ctx: MutationContext<'_, String>, input: CreateArticle) -> Result<Article, Error> {
    ctx.db
        .insert(PublicationInsert {
            slug: input.publication.key().clone(),
            title: "Must roll back".into(),
        })
        .await?;
    let first = ctx
        .db
        .insert(ArticleInsert {
            publication: input.publication.clone(),
            title: input.title.clone(),
            published: None,
            ordinal: None,
            excerpt: None,
            origin: None,
        })
        .await?;
    // The later unique constraint must roll back the publication and first row.
    ctx.db
        .insert(ArticleInsert {
            publication: input.publication,
            title: input.title,
            published: None,
            ordinal: None,
            excerpt: None,
            origin: None,
        })
        .await?;
    Ok(first)
}

#[derive(hulladb::Input)]
struct Edit {
    id: Id<Article>,
    version: i64,
    excerpt: Option<String>,
}
#[mutation(name = "articles.edit")]
async fn edit(ctx: MutationContext<'_, String>, input: Edit) -> Result<Article, Error> {
    let existing = ctx.db.get(input.id).await?.ok_or(Error::NotFound)?;
    if existing.row.publication.key() != ctx.identity {
        return Err(Error::Forbidden);
    }
    ctx.db
        .update(
            input.id,
            input.version,
            ArticlePatch {
                excerpt: Some(input.excerpt),
                ..ArticlePatch::default()
            },
        )
        .await
}
#[query(name = "articles.first_version")]
async fn first_version(ctx: QueryContext<'_, String>) -> Result<i64, Error> {
    ctx.db
        .select::<Article>()
        .where_eq(Article::fields().publication, Id::new(ctx.identity.clone()))
        .take(1)
        .versioned()
        .await?
        .first()
        .map(|row| row.version)
        .ok_or(Error::NotFound)
}

struct Policy(Arc<AtomicBool>);
impl Authorization<String> for Policy {
    fn authorize<'a>(
        &'a self,
        _: QueryContext<'a, String>,
        _: Invocation,
        _: &'a Json,
    ) -> FunctionFuture<'a, ()> {
        Box::pin(async move {
            if !self.0.load(Ordering::SeqCst) {
                return Err(Error::Forbidden);
            }
            Ok(())
        })
    }
}
fn registry(allowed: &Arc<AtomicBool>) -> Registry<String> {
    let mut registry = Registry::new();
    pending::register(&mut registry, Policy(allowed.clone())).unwrap();
    create::register(&mut registry, Policy(allowed.clone())).unwrap();
    fail::register(&mut registry, Policy(allowed.clone())).unwrap();
    edit::register(&mut registry, Policy(allowed.clone())).unwrap();
    first_version::register(&mut registry, Policy(allowed.clone())).unwrap();
    registry
}
fn url() -> String {
    std::env::var("HULLADB_TEST_URL")
        .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into())
}
fn input(owner: &str, title: &str) -> CreateArticle {
    CreateArticle {
        publication: Id::new(owner.into()),
        title: title.into(),
        ordinal: i64::MAX,
        notes: None,
        published: false,
    }
}
async fn fixture() -> (ManagedSchema, Context<String>, Arc<AtomicBool>) {
    let schema = ManagedSchema::new(
        format!("rust_authoring_{}", Uuid::now_v7().simple()),
        hulladb::schema!(Publication, Article, Counter).unwrap(),
    )
    .unwrap();
    let mut store = ManagedStore::connect(&url(), schema.clone()).await.unwrap();
    store.initialize().await.unwrap();
    let context = Context {
        scope: Uuid::now_v7(),
        principal: Uuid::now_v7(),
        authorization_scope: Uuid::now_v7(),
        identity: "daily".into(),
    };
    store.create_scope(context.scope).await.unwrap();
    (schema, context, Arc::new(AtomicBool::new(true)))
}
async fn cleanup(schema: &ManagedSchema) {
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
async fn runtime(
    schema: &ManagedSchema,
    context: Context<String>,
    allowed: &Arc<AtomicBool>,
) -> Arc<LocalRuntime<String>> {
    Arc::new(
        LocalRuntime::connect(
            &url(),
            schema.clone(),
            registry(allowed),
            context,
            Duration::from_millis(10),
        )
        .await
        .unwrap(),
    )
}

#[tokio::test]
async fn direct_bindings_fail_without_a_request_scope() {
    assert!(matches!(pending().await, Err(Error::InvalidInput(_))));
    assert!(matches!(
        create(input("daily", "Must not run")).await,
        Err(Error::InvalidInput(_))
    ));
    assert!(pending::watch().await.is_err());
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make rust-authoring-test"]
async fn derived_functions_execute_real_typed_transactions_and_enforce_versions() {
    let (schema, context, allowed) = fixture().await;
    let runtime = runtime(&schema, context.clone(), &allowed).await;
    hulladb::scope(runtime.clone(), async {
        let created = create(input("daily", "First")).await.unwrap();
        assert_eq!(created.ordinal, i64::MAX);
        assert_eq!(
            created.origin,
            Uuid::parse_str("aaaaaaaa-0000-0000-0000-000000000001").unwrap()
        );
        assert_eq!(pending().await.unwrap()[0].id(), created.id());
        assert_eq!(first_version().await.unwrap(), 1);
        let changed = edit(Edit {
            id: created.id(),
            version: 1,
            excerpt: Some("Summary".into()),
        })
        .await
        .unwrap();
        assert_eq!(changed.excerpt.as_deref(), Some("Summary"));
        let cleared = edit(Edit {
            id: created.id(),
            version: 2,
            excerpt: None,
        })
        .await
        .unwrap();
        assert_eq!(cleared.excerpt, None);
        assert!(matches!(
            edit(Edit {
                id: created.id(),
                version: 1,
                excerpt: None
            })
            .await,
            Err(Error::Conflict)
        ));
        assert!(matches!(
            fail(input("rollback", "Duplicate")).await,
            Err(Error::Conflict)
        ));
        // If the publication escaped rollback, this second attempt would fail
        // on its first statement rather than the original later unique failure.
        assert!(matches!(
            fail(input("rollback", "Duplicate")).await,
            Err(Error::Conflict)
        ));
        assert_eq!(pending().await.unwrap().len(), 1);
        assert!(runtime.pending().unwrap().is_empty());
    })
    .await;
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let count: i64 = client
        .query_one(
            &format!(
                "SELECT count(*) FROM \"{}\".t0 WHERE _scope=$1 AND f0=$2",
                schema.namespace()
            ),
            &[&context.scope, &"rollback"],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        count, 0,
        "an earlier write escaped the failed handler transaction"
    );
    connection.abort();
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make rust-authoring-test"]
async fn concurrent_execution_scopes_and_owned_watches_never_mix_identities() {
    let (schema, alice, allowed) = fixture().await;
    let bob = Context {
        principal: Uuid::now_v7(),
        authorization_scope: Uuid::now_v7(),
        identity: "weekly".into(),
        ..alice.clone()
    };
    let alice = runtime(&schema, alice, &allowed).await;
    let bob = runtime(&schema, bob, &allowed).await;
    let (a, b) = tokio::join!(
        hulladb::scope(alice.clone(), async {
            create(input("daily", "Alice")).await.unwrap();
            pending().await.unwrap()
        }),
        hulladb::scope(bob.clone(), async {
            create(input("weekly", "Bob")).await.unwrap();
            pending().await.unwrap()
        }),
    );
    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);
    assert_eq!(a[0].publication.key(), "daily");
    assert_eq!(b[0].publication.key(), "weekly");
    hulladb::scope(alice.clone(), async {
        let mut subscription = pending::watch().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                subscription.changed().await.unwrap();
                if matches!(&*subscription.state(), QueryState::Ready { .. }) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        if let QueryState::Ready { data, .. } = &*subscription.state() {
            assert_eq!(data[0].publication.key(), "daily");
        }
        create(input("daily", "Later")).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                subscription.changed().await.unwrap();
                if matches!(&*subscription.state(), QueryState::Ready { data, .. } if data.len()==2)
                {
                    break;
                }
            }
        })
        .await
        .unwrap();
        alice.retire();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(&*subscription.state(), QueryState::Error(_)) {
                    break;
                }
                subscription.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(matches!(pending().await, Err(Error::Forbidden)));
        drop(subscription);
    })
    .await;
    hulladb::scope(bob, async {
        assert_eq!(pending().await.unwrap().len(), 1);
    })
    .await;
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make rust-authoring-test"]
async fn raw_dispatch_and_typed_storage_reject_contract_mismatches() {
    let (schema, context, allowed) = fixture().await;
    let mut store = ManagedStore::connect(&url(), schema.clone()).await.unwrap();
    store.verify().await.unwrap();
    let registry = registry(&allowed);
    let request = Uuid::now_v7();
    let mut encoded = input("daily", "Bad").encode().unwrap();
    encoded["extra"] = json!(true);
    assert!(matches!(
        store
            .mutate(&registry, &context, request, create::NAME, encoded)
            .await,
        Err(Error::InvalidInput(_))
    ));
    assert!(store.receipt(&context, request).await.unwrap().is_none());
    assert!(matches!(
        store
            .query(&registry, &context, create::NAME, Json::Null)
            .await,
        Err(Error::WrongFunctionKind)
    ));
    let mut schema_decl = hulladb::schema::SchemaDeclaration::new(vec![
        Publication::declaration(),
        Article::declaration(),
        Counter::declaration(),
    ]);
    schema_decl.references = Article::references();
    schema_decl.tables[1]
        .fields
        .iter_mut()
        .find(|field| field.name == "displayTitle")
        .unwrap()
        .name = "wrongTitle".into();
    schema_decl.tables[1].unique_constraints[0].fields[1] = "wrongTitle".into();
    let wrong = ManagedSchema::new(
        format!("rust_mismatch_{}", Uuid::now_v7().simple()),
        schema_decl.normalize().unwrap(),
    )
    .unwrap();
    let mut setup = ManagedStore::connect(&url(), wrong.clone()).await.unwrap();
    setup.initialize().await.unwrap();
    setup.create_scope(context.scope).await.unwrap();
    let wrong_runtime = runtime(&wrong, context, &allowed).await;
    hulladb::scope(wrong_runtime, async {
        assert!(matches!(pending().await, Err(Error::SchemaMismatch)));
    })
    .await;
    cleanup(&wrong).await;
    cleanup(&schema).await;
}

#[mutation(name = "articles.nested")]
async fn nested(_: MutationContext<'_, String>, input: CreateArticle) -> Result<Article, Error> {
    create(input).await
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make rust-authoring-test"]
async fn nested_registered_calls_fail_instead_of_reentering_a_transaction() {
    let (schema, context, allowed) = fixture().await;
    let mut registry = registry(&allowed);
    nested::register(&mut registry, Policy(allowed)).unwrap();
    let runtime = Arc::new(
        LocalRuntime::connect(
            &url(),
            schema.clone(),
            registry,
            context,
            Duration::from_millis(10),
        )
        .await
        .unwrap(),
    );
    hulladb::scope(runtime.clone(), async {
        let outcome = tokio::time::timeout(
            Duration::from_secs(2),
            nested(input("daily", "Must fail closed")),
        )
        .await
        .unwrap();
        assert!(matches!(outcome, Err(Error::InvalidInput(_))));
        assert!(pending().await.unwrap().is_empty());
        assert!(runtime.pending().unwrap().is_empty());
    })
    .await;
    cleanup(&schema).await;
}

struct RecoveryIdentity {
    started: Arc<tokio::sync::Notify>,
    attempts: Arc<AtomicUsize>,
}
struct RecoveryPolicy;
impl Authorization<RecoveryIdentity> for RecoveryPolicy {
    fn authorize<'a>(
        &'a self,
        _: QueryContext<'a, RecoveryIdentity>,
        _: Invocation,
        _: &'a Json,
    ) -> FunctionFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}
#[mutation(name = "publications.interrupted")]
async fn interrupted(
    ctx: MutationContext<'_, RecoveryIdentity>,
    input: CreateArticle,
) -> Result<Publication, Error> {
    let row = ctx
        .db
        .insert(PublicationInsert {
            slug: input.publication.key().clone(),
            title: input.title,
        })
        .await?;
    if ctx.identity.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
        ctx.identity.started.notify_one();
        std::future::pending::<()>().await;
    }
    Ok(row)
}

#[tokio::test]
#[ignore = "requires local PostgreSQL; run make rust-authoring-test"]
async fn canceled_mutation_recovers_the_same_request_without_duplicate_writes() {
    let (schema, context, _) = fixture().await;
    let started = Arc::new(tokio::sync::Notify::new());
    let attempts = Arc::new(AtomicUsize::new(0));
    let identity = RecoveryIdentity {
        started: started.clone(),
        attempts: attempts.clone(),
    };
    // The identity is shared only to observe the real handler's first write.
    // Cancellation must roll it back before recovery retries the same request.
    let mut registry = Registry::new();
    interrupted::register(&mut registry, RecoveryPolicy).unwrap();
    let context = Context {
        scope: context.scope,
        principal: context.principal,
        authorization_scope: context.authorization_scope,
        identity,
    };
    let runtime = Arc::new(
        LocalRuntime::connect(
            &url(),
            schema.clone(),
            registry,
            context,
            Duration::from_millis(10),
        )
        .await
        .unwrap(),
    );
    let task_runtime = runtime.clone();
    let call = tokio::spawn(async move {
        hulladb::scope(task_runtime, interrupted(input("daily", "Recover me"))).await
    });
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    let intent = runtime.pending().unwrap().pop().unwrap();
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    let receipts = runtime.recover_pending().await.unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].request_id, intent.request_id);
    assert_eq!(
        Publication::decode(&receipts[0].result).unwrap().title,
        "Recover me"
    );
    assert!(runtime.pending().unwrap().is_empty());
    assert!(runtime.recover_pending().await.unwrap().is_empty());
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    cleanup(&schema).await;
}

#[test]
fn derived_mutation_registration_checks_retry_budgets_before_registering_the_handler() {
    use hulladb::{MutationConcurrency, RetryOptions};
    let mut registry = Registry::new();
    let permission = Arc::new(AtomicBool::new(true));
    let invalid = MutationConcurrency::Serializable(RetryOptions {
        max_attempts: 0,
        ..RetryOptions::default()
    });
    assert!(matches!(
        create::register_with_concurrency(&mut registry, Policy(permission.clone()), invalid),
        Err(Error::InvalidInput(_))
    ));
    create::register_with_concurrency(
        &mut registry,
        Policy(permission),
        MutationConcurrency::Serializable(RetryOptions::default()),
    )
    .unwrap();
}
