use std::{sync::Arc, time::Duration};

use hulladb_postgres::managed::{
    Authorization, Context, Error, FunctionContract, FunctionFuture, Invocation, ManagedSchema,
    ManagedStore, Mutation, MutationContext, Query, QueryContext, Registry, RoundLimits,
    RoundQuery, Select,
};
use hulladb_schema::{
    FieldDefinition, RecordValues, ScalarType, SchemaDeclaration, TableDeclaration, Value,
};
use serde_json::{Value as Json, json};
use tokio::sync::Notify;
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
struct Permission;
impl Authorization<()> for Permission {
    fn authorize<'a>(
        &'a self,
        ctx: QueryContext<'a, ()>,
        _: Invocation,
        _: &'a Json,
    ) -> FunctionFuture<'a, ()> {
        Box::pin(async move {
            if ctx
                .db
                .get("permissions", Value::String("access".into()))
                .await?
                .is_some_and(|row| row.values["allowed"] == Value::Boolean(true))
            {
                Ok(())
            } else {
                Err(Error::Forbidden)
            }
        })
    }
}
#[derive(Clone)]
enum Read {
    Count,
    List,
    Constant,
    EarlyFailure,
    SqlFailure,
    Pause(Arc<Notify>, Arc<Notify>),
    Slow(Duration),
}
impl FunctionContract for Read {
    fn validate_input(&self, _: &Json) -> Result<(), Error> {
        Ok(())
    }
    fn validate_result(&self, _: &Json) -> Result<(), Error> {
        Ok(())
    }
}
impl Query<()> for Read {
    fn call<'a>(&'a self, ctx: QueryContext<'a, ()>, _: &'a Json) -> FunctionFuture<'a, Json> {
        Box::pin(async move {
            if matches!(self, Self::Constant) {
                return Ok(json!(0));
            }
            if matches!(self, Self::EarlyFailure) {
                return Err(Error::InvalidInput("fails before reading".into()));
            }
            if let Self::Slow(duration) = self {
                tokio::time::sleep(*duration).await;
            }
            let rows = ctx
                .db
                .select(
                    "items",
                    Select {
                        limit: 1000,
                        ..Select::default()
                    },
                )
                .await?;
            if let Self::Pause(entered, resume) = self {
                entered.notify_one();
                resume.notified().await;
            }
            if matches!(self, Self::SqlFailure) {
                ctx.db.select("broken", Select::default()).await?;
            }
            if matches!(self, Self::List) {
                Ok(json!(
                    rows.iter()
                        .map(|row| match row.values["id"] {
                            Value::Int64(id) => id,
                            _ => panic!("invalid key"),
                        })
                        .collect::<Vec<_>>()
                ))
            } else {
                Ok(json!(rows.len()))
            }
        })
    }
}
struct Write;
impl FunctionContract for Write {
    fn validate_input(&self, _: &Json) -> Result<(), Error> {
        Ok(())
    }
    fn validate_result(&self, _: &Json) -> Result<(), Error> {
        Ok(())
    }
}
impl Mutation<()> for Write {
    fn call<'a>(
        &'a self,
        ctx: MutationContext<'a, ()>,
        input: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        Box::pin(async move {
            if let Some(allowed) = input.as_bool() {
                if let Some(row) = ctx
                    .db
                    .get("permissions", Value::String("access".into()))
                    .await?
                {
                    ctx.db
                        .update(
                            "permissions",
                            Value::String("access".into()),
                            row.version,
                            RecordValues::from([("allowed".into(), Value::Boolean(allowed))]),
                        )
                        .await?;
                } else {
                    ctx.db
                        .insert(
                            "permissions",
                            RecordValues::from([
                                ("id".into(), Value::String("access".into())),
                                ("allowed".into(), Value::Boolean(allowed)),
                            ]),
                        )
                        .await?;
                }
            } else {
                ctx.db
                    .insert(
                        "items",
                        RecordValues::from([("id".into(), Value::Int64(input.as_i64().unwrap()))]),
                    )
                    .await?;
            }
            Ok(Json::Null)
        })
    }
}
async fn fixture() -> (ManagedStore, Registry<()>, Context<()>, Uuid) {
    let schema = SchemaDeclaration::new(vec![
        TableDeclaration::new("items", vec![FieldDefinition::new("id", ScalarType::Int64)]),
        TableDeclaration::new(
            "permissions",
            vec![
                FieldDefinition::new("id", ScalarType::String),
                FieldDefinition::new("allowed", ScalarType::Boolean),
            ],
        ),
        TableDeclaration::new("broken", vec![]),
    ])
    .normalize()
    .unwrap();
    let schema = ManagedSchema::new(format!("round_{}", Uuid::now_v7().simple()), schema).unwrap();
    let mut config: tokio_postgres::Config = url().parse().unwrap();
    config.application_name(schema.namespace());
    let mut store = ManagedStore::connect_config(config, schema).await.unwrap();
    store.initialize().await.unwrap();
    let context = Context {
        scope: Uuid::now_v7(),
        principal: Uuid::now_v7(),
        authorization_scope: Uuid::now_v7(),
        identity: (),
    };
    store.create_scope(context.scope).await.unwrap();
    let mut registry = Registry::new();
    registry.query("count", Read::Count, Permission).unwrap();
    registry.query("list", Read::List, Permission).unwrap();
    registry.mutation("write", Write, Permit).unwrap();
    store
        .mutate(&registry, &context, Uuid::now_v7(), "write", json!(true))
        .await
        .unwrap();
    let source = store.managed_head(&context).await.unwrap().0;
    (store, registry, context, source)
}
fn member(key: &str, operation: &str) -> RoundQuery {
    RoundQuery {
        key: key.into(),
        operation: operation.into(),
        input: Json::Null,
        page: None,
    }
}
async fn sql(statement: &str) {
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    client.batch_execute(statement).await.unwrap();
    connection.abort();
}
async fn cleanup(store: &ManagedStore) {
    sql(&format!(
        "DROP SCHEMA \"{}\" CASCADE",
        store.schema().namespace()
    ))
    .await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; shared query snapshot proof"]
async fn writes_between_members_cannot_mix_count_and_list_snapshots() {
    let (mut store, mut registry, context, source) = fixture().await;
    let mut writer = ManagedStore::connect(&url(), store.schema().clone())
        .await
        .unwrap();
    writer.verify().await.unwrap();
    let entered = Arc::new(Notify::new());
    let resume = Arc::new(Notify::new());
    registry
        .query(
            "paused_count",
            Read::Pause(entered.clone(), resume.clone()),
            Permission,
        )
        .unwrap();
    let queries = [member("count", "paused_count"), member("list", "list")];
    let (round, ()) = tokio::join!(
        store.query_round(
            &registry,
            &context,
            source,
            &queries,
            RoundLimits::default()
        ),
        async {
            entered.notified().await;
            writer
                .mutate(&registry, &context, Uuid::now_v7(), "write", json!(1))
                .await
                .unwrap();
            resume.notify_one();
        }
    );
    let round = round.unwrap();
    assert_eq!(round.results[0].result.as_ref().unwrap(), &json!(0));
    assert_eq!(round.results[1].result.as_ref().unwrap(), &json!([]));
    assert_eq!(round.revision.sequence, 1);
    let next = store
        .query_round(
            &registry,
            &context,
            source,
            &[member("count", "count"), member("list", "list")],
            RoundLimits::default(),
        )
        .await
        .unwrap();
    assert_eq!(next.results[0].result.as_ref().unwrap(), &json!(1));
    assert_eq!(next.results[1].result.as_ref().unwrap(), &json!([1]));
    assert_eq!(next.revision.sequence, 2);
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; shared query snapshot proof"]
async fn sql_failure_isolated_from_siblings_and_failed_empty_permission_reads_stay_dependencies() {
    let (mut store, mut registry, context, source) = fixture().await;
    registry
        .query("sql_failure", Read::SqlFailure, Permission)
        .unwrap();
    registry
        .query("early_failure", Read::EarlyFailure, Permit)
        .unwrap();
    sql(&format!("DROP TABLE \"{}\".t2", store.schema().namespace())).await;
    let round = store
        .query_round(
            &registry,
            &context,
            source,
            &[
                member("bad", "sql_failure"),
                member("early", "early_failure"),
                member("good", "list"),
            ],
            RoundLimits::default(),
        )
        .await
        .unwrap();
    assert!(
        matches!(&round.results[0].result, Err(Error::Database(error)) if error.code() == Some(&tokio_postgres::error::SqlState::UNDEFINED_TABLE))
    );
    assert!(matches!(
        &round.results[1].result,
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(round.results[2].result.as_ref().unwrap(), &json!([]));
    assert_eq!(
        round.results[0].tables,
        ["items".into(), "permissions".into(), "broken".into()].into()
    );
    assert_eq!(round.results[1].tables, round.results[0].tables);
    assert_eq!(
        round.results[2].tables,
        ["items".into(), "permissions".into()].into()
    );
    store
        .mutate(&registry, &context, Uuid::now_v7(), "write", json!(false))
        .await
        .unwrap();
    let denied = store
        .query_round(
            &registry,
            &context,
            source,
            &[member("count", "count"), member("list", "list")],
            RoundLimits::default(),
        )
        .await
        .unwrap();
    assert!(
        denied
            .results
            .iter()
            .all(|member| matches!(member.result, Err(Error::Forbidden)))
    );
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; shared query snapshot proof"]
async fn elapsed_deadline_result_budget_and_source_changes_discard_the_complete_round() {
    let (mut store, mut registry, context, source) = fixture().await;
    registry
        .query("slow", Read::Slow(Duration::from_millis(60)), Permission)
        .unwrap();
    let started = tokio::time::Instant::now();
    let result = store
        .query_round(
            &registry,
            &context,
            source,
            &[member("a", "slow"), member("b", "slow")],
            RoundLimits {
                timeout: Duration::from_millis(90),
                ..RoundLimits::default()
            },
        )
        .await;
    assert!(matches!(result, Err(Error::TransactionDeadline)));
    assert!(started.elapsed() < Duration::from_secs(1));
    // Cancellation queues rollback; the same connection must be reusable after it.
    let next = store
        .query_round(
            &registry,
            &context,
            source,
            &[member("count", "count")],
            RoundLimits::default(),
        )
        .await
        .unwrap();
    assert_eq!(next.results[0].result.as_ref().unwrap(), &json!(0));
    assert!(matches!(
        store
            .query_round(
                &registry,
                &context,
                source,
                &[member("a", "list"), member("b", "list")],
                RoundLimits {
                    max_result_bytes: 3,
                    ..RoundLimits::default()
                }
            )
            .await,
        Err(Error::QuotaExceeded)
    ));
    assert!(matches!(
        store
            .query_round(
                &registry,
                &context,
                Uuid::now_v7(),
                &[member("a", "list")],
                RoundLimits::default()
            )
            .await,
        Err(Error::SourceMismatch)
    ));
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; shared query snapshot proof"]
async fn connection_loss_discards_successful_prefix_instead_of_publishing_partial_round() {
    let (mut store, mut registry, context, source) = fixture().await;
    let entered = Arc::new(Notify::new());
    let resume = Arc::new(Notify::new());
    registry
        .query(
            "paused",
            Read::Pause(entered.clone(), resume.clone()),
            Permission,
        )
        .unwrap();
    let queries = [member("good", "list"), member("paused", "paused")];
    let namespace = store.schema().namespace().to_owned();
    let (result, ()) = tokio::join!(
        store.query_round(
            &registry,
            &context,
            source,
            &queries,
            RoundLimits::default()
        ),
        async {
            entered.notified().await;
            sql(&format!("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name='{namespace}'")).await;
            resume.notify_one();
        }
    );
    assert!(matches!(result, Err(Error::Database(_))));
    let mut cleanup_store = ManagedStore::connect(&url(), store.schema().clone())
        .await
        .unwrap();
    cleanup_store.verify().await.unwrap();
    cleanup(&cleanup_store).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; shared query snapshot proof"]
async fn sustained_writes_do_not_restart_rounds_and_member_cost_is_measured() {
    let (mut store, mut registry, context, source) = fixture().await;
    let mut writer = ManagedStore::connect(&url(), store.schema().clone())
        .await
        .unwrap();
    writer.verify().await.unwrap();
    let queries = [member("count", "count"), member("list", "list")];
    let started = tokio::time::Instant::now();
    let ((), ()) = tokio::join!(
        async {
            for _ in 0..20 {
                let round = store
                    .query_round(
                        &registry,
                        &context,
                        source,
                        &queries,
                        RoundLimits::default(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    round.results[0].result.as_ref().unwrap().as_u64().unwrap() as usize,
                    round.results[1]
                        .result
                        .as_ref()
                        .unwrap()
                        .as_array()
                        .unwrap()
                        .len()
                );
            }
        },
        async {
            for id in 1..=20 {
                writer
                    .mutate(&registry, &context, Uuid::now_v7(), "write", json!(id))
                    .await
                    .unwrap();
            }
        }
    );
    println!(
        "sustained_writes rounds=20 writes=20 elapsed_us={}",
        started.elapsed().as_micros()
    );
    registry
        .query("slow", Read::Slow(Duration::from_millis(10)), Permission)
        .unwrap();
    for count in [1usize, 8, 32] {
        let queries = (0..count)
            .map(|n| member(&n.to_string(), "list"))
            .collect::<Vec<_>>();
        let started = tokio::time::Instant::now();
        let round = store
            .query_round(
                &registry,
                &context,
                source,
                &queries,
                RoundLimits::default(),
            )
            .await
            .unwrap();
        println!(
            "round_cost members={count} elapsed_us={} result_bytes={}",
            started.elapsed().as_micros(),
            round.result_bytes
        );
    }
    let queries = [member("slow", "slow"), member("list", "list")];
    let started = tokio::time::Instant::now();
    store
        .query_round(
            &registry,
            &context,
            source,
            &queries,
            RoundLimits::default(),
        )
        .await
        .unwrap();
    println!(
        "round_head_of_line slow_member_ms=10 members=2 elapsed_us={}",
        started.elapsed().as_micros()
    );
    cleanup(&store).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; shared query snapshot proof"]
async fn active_server_sql_is_bounded_and_connection_cleanup_precedes_next_round() {
    let (mut store, mut registry, context, source) = fixture().await;
    store
        .mutate(&registry, &context, Uuid::now_v7(), "write", json!(1))
        .await
        .unwrap();
    registry
        .query("constant", Read::Constant, Permission)
        .unwrap();
    let namespace = store.schema().namespace();
    sql(&format!("ALTER TABLE \"{namespace}\".t0 RENAME TO original_items; CREATE VIEW \"{namespace}\".t0 AS SELECT items.*, pg_sleep(0.3)::text AS delay FROM \"{namespace}\".original_items AS items")).await;
    let started = tokio::time::Instant::now();
    let round = store
        .query_round(
            &registry,
            &context,
            source,
            &[member("prefix", "constant"), member("slow_sql", "list")],
            RoundLimits {
                timeout: Duration::from_millis(80),
                ..RoundLimits::default()
            },
        )
        .await;
    assert!(matches!(round, Err(Error::TransactionDeadline)));
    let next = store
        .query_round(
            &registry,
            &context,
            source,
            &[member("constant", "constant")],
            RoundLimits::default(),
        )
        .await
        .unwrap();
    assert_eq!(next.results[0].result.as_ref().unwrap(), &json!(0));
    assert!(started.elapsed() < Duration::from_secs(1));
    cleanup(&store).await;
}
