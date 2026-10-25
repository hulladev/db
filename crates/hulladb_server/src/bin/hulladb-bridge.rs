#[tokio::main]
async fn main() {
    hulladb_postgres::hosting::bridge::main().await;
}
