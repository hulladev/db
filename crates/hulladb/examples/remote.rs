//! Client-only build: cargo run -p hulladb --no-default-features --features remote --example remote
use hulladb::{
    Error, Id, QueryState,
    remote::{Endpoint, Options, RecoveryStore, RemoteRuntime, Session},
};
#[cfg(feature = "server")]
use hulladb::{MutationContext, QueryContext};
use std::sync::Arc;

// This module can be shared with the server. In client-only builds the function
// attributes omit bodies, context/identity types and registry adapters entirely.
#[derive(hulladb::Table, Debug)]
#[table(
    name = "tasks",
    generation = "uuidV4",
    insert_key_policy = "generatedOnly"
)]
pub struct Task {
    pub id: Id<Task>,
    pub title: String,
    #[field(default = false)]
    pub done: bool,
}
#[derive(hulladb::Input)]
pub struct CreateTask {
    pub title: String,
}
#[hulladb::query(name = "tasks.pending")]
pub async fn pending(ctx: QueryContext<'_, String>) -> Result<Vec<Task>, Error> {
    ctx.db
        .select::<Task>()
        .where_eq(Task::fields().done, false)
        .take(100)
        .await
}
#[hulladb::mutation(name = "tasks.create")]
pub async fn create(ctx: MutationContext<'_, String>, input: CreateTask) -> Result<Task, Error> {
    ctx.db
        .insert(TaskInsert {
            title: input.title,
            done: None,
        })
        .await
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint: Endpoint = std::env::var("HULLADB_ORIGIN")?.parse()?;
    let bearer = std::env::var("HULLADB_BEARER")?;
    // Supply these from the trusted authenticated application session.
    let session = Session {
        scope: std::env::var("HULLADB_SCOPE")?.parse()?,
        principal: std::env::var("HULLADB_PRINCIPAL")?.parse()?,
        authorization_scope: std::env::var("HULLADB_AUTHORIZATION_SCOPE")?.parse()?,
    };
    let storage = RecoveryStore::open("functions.sqlite").await?;
    let runtime = Arc::new(
        RemoteRuntime::connect(endpoint, &bearer, session, storage, Options::default()).await?,
    );
    runtime.recover_pending(&[create::contract()]).await?;
    hulladb::scope(runtime.clone(), async {
        println!("{:?}", pending().await?);
        let mut subscription = pending::watch().await?;
        loop {
            match &*subscription.state() {
                QueryState::Ready {
                    data, connected, ..
                } => println!("connected={connected}: {data:?}"),
                QueryState::Error(error) => {
                    eprintln!("{error}");
                    break;
                }
                QueryState::Loading => {}
            }
            subscription.changed().await?;
        }
        Ok::<_, Error>(())
    })
    .await?;
    runtime.retire().await?;
    Ok(())
}
