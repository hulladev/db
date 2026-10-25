#[tokio::main]
async fn main() {
    hulladb_postgres::hosting::service::main().await;
}
