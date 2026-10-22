//! Compile-checked authoring example. Run with HULLADB_TEST_URL pointing at an
//! owned development database; the example creates and removes its own namespace.
use std::{sync::Arc, time::Duration};

use hulladb::{
    Authorization, Context, Error, FunctionFuture, Id, Input, Invocation, LocalRuntime,
    ManagedSchema, ManagedStore, MutationContext, QueryContext, Registry, Table, Uuid, mutation,
    query,
};
use serde_json::Value;

#[derive(Table)]
#[table(name = "tasks")]
pub struct Task {
    pub id: Id<Task>,
    pub title: String,
    #[field(default = false)]
    pub completed: bool,
    pub owner: String,
}

#[derive(Input)]
pub struct CreateTask {
    pub title: String,
}

#[query(name = "tasks.pending")]
pub async fn pending(ctx: QueryContext<'_, String>) -> Result<Vec<Task>, Error> {
    ctx.db
        .select::<Task>()
        .where_eq(Task::fields().owner, ctx.identity.clone())
        .where_eq(Task::fields().completed, false)
        .take(50)
        .await
}

#[mutation(name = "tasks.create")]
pub async fn create(ctx: MutationContext<'_, String>, input: CreateTask) -> Result<Task, Error> {
    let title = input.title.trim().to_owned();
    if title.is_empty() {
        return Err(Error::InvalidInput("title is required".into()));
    }
    ctx.db
        .insert(TaskInsert {
            title,
            completed: None,
            owner: ctx.identity.clone(),
        })
        .await
}

struct OwnerPolicy;
impl Authorization<String> for OwnerPolicy {
    fn authorize<'a>(
        &'a self,
        ctx: QueryContext<'a, String>,
        _: Invocation,
        _: &'a Value,
    ) -> FunctionFuture<'a, ()> {
        Box::pin(async move {
            if ctx.identity.is_empty() {
                return Err(Error::Forbidden);
            }
            Ok(())
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("HULLADB_TEST_URL")
        .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into());
    let schema = ManagedSchema::new(
        format!("authoring_example_{}", Uuid::now_v7().simple()),
        hulladb::schema!(Task)?,
    )?;
    let mut store = ManagedStore::connect(&url, schema.clone()).await?;
    store.initialize().await?;
    // In a host, construct this from verified application authentication.
    let context = Context {
        scope: Uuid::now_v7(),
        principal: Uuid::now_v7(),
        authorization_scope: Uuid::now_v7(),
        identity: "example-user".to_owned(),
    };
    store.create_scope(context.scope).await?;
    let mut registry = Registry::new();
    pending::register(&mut registry, OwnerPolicy)?;
    create::register(&mut registry, OwnerPolicy)?;
    let runtime = Arc::new(
        LocalRuntime::connect(
            &url,
            schema.clone(),
            registry,
            context,
            Duration::from_millis(100),
        )
        .await?,
    );
    hulladb::scope(runtime.clone(), async {
        let task = create(CreateTask {
            title: "Ship it".into(),
        })
        .await?;
        assert_eq!(pending().await?[0].id, task.id);
        let mut subscription = pending::watch().await?;
        subscription.changed().await?;
        drop(subscription);
        Ok::<_, Error>(())
    })
    .await?;
    runtime.retire();
    let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls).await?;
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!("DROP SCHEMA \"{}\" CASCADE", schema.namespace()))
        .await?;
    connection.abort();
    Ok(())
}
