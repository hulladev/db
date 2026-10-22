use hulladb::{Id, QueryContext, Table};
#[derive(Table)]
struct Note { id: Id<Note>, title: String }
async fn invalid(ctx: QueryContext<'_>) {
    ctx.db.select::<Note>().where_eq(Note::fields().title, true).await.unwrap();
}
fn main() {}
