//! Replace this module with the application's own schema, functions and policies.
use hulladb::{
    Authorization, Error, FunctionFuture, Id, Input, Invocation, MutationContext, QueryContext,
    Registry, Table, mutation, query,
};
use serde_json::Value;
#[derive(Table)]
#[table(name = "entries")]
#[unique(name = "owner_title", fields(owner, title))]
pub struct Entry {
    pub id: Id<Entry>,
    pub title: String,
    pub owner: String,
}
#[derive(Input)]
pub struct CreateEntry {
    pub title: String,
}
#[query(name = "entries.list")]
pub async fn list(ctx: QueryContext<'_, String>) -> Result<Vec<Entry>, Error> {
    ctx.db
        .select::<Entry>()
        .where_eq(Entry::fields().owner, ctx.identity.clone())
        .take(100)
        .await
}
#[mutation(name = "entries.create")]
pub async fn create(ctx: MutationContext<'_, String>, input: CreateEntry) -> Result<Entry, Error> {
    let title = input.title.trim().to_owned();
    if title.is_empty() || title.len() > 4096 {
        return Err(Error::InvalidInput(
            "title must contain 1..4096 bytes".into(),
        ));
    }
    ctx.db
        .insert(EntryInsert {
            title,
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
pub fn registry() -> Result<Registry<String>, Error> {
    let mut registry = Registry::new();
    list::register(&mut registry, OwnerPolicy)?;
    create::register(&mut registry, OwnerPolicy)?;
    Ok(registry)
}
