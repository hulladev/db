use hulladb::{Error, MutationContext, mutation};
#[mutation]
async fn create(_: MutationContext<'_>) -> Result<bool, Error> { Ok(true) }
async fn invalid() { create::watch().await.unwrap(); }
fn main() {}
