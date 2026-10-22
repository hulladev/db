use hulladb::{Id, QueryContext, Table};
#[derive(Table)]
struct Note { id: Id<Note>, title: String }
#[derive(Table)]
struct Other { id: Id<Other>, title: String }
async fn invalid(ctx: QueryContext<'_>, id: Id<Other>) {
    ctx.db.get::<Note>(id).await.unwrap();
}
fn main() {}
