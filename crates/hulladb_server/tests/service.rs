#![cfg(feature = "typescript")]

use tokio::process::Command;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires PostgreSQL and Bun"]
async fn generated_application_service_serves_calls_auth_and_live_direct_writes() {
    generated_application_service("BUN", "bun").await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL, Node.js and Bun for generation"]
async fn generated_application_service_serves_calls_auth_and_live_direct_writes_under_node() {
    generated_application_service("NODE", "node").await;
}

async fn generated_application_service(runtime_variable: &str, fallback: &str) {
    let url = std::env::var("HULLADB_TEST_URL").expect("set HULLADB_TEST_URL");
    let namespace = format!("service_{}", Uuid::now_v7().simple());
    let result = Command::new(std::env::var(runtime_variable).unwrap_or_else(|_| fallback.into()))
        .arg("--conditions=hulladb-source")
        .arg("tests/service.integration.ts")
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages/db"))
        .env("HULLADB_TEST_URL", &url)
        .env("HULLADB_TEST_NAMESPACE", &namespace)
        .env("HULLADB_ENGINE", env!("CARGO_BIN_EXE_hulladb-bridge"))
        .env("HULLADB_SERVICE", env!("CARGO_BIN_EXE_hulladb-service"))
        .kill_on_drop(true)
        .output()
        .await
        .expect("launch runtime service integration");
    let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
        .await
        .unwrap_or_else(|error| {
            panic!(
                "test database cleanup failed: {error}; fixture output: {}\n{}",
                String::from_utf8_lossy(&result.stderr),
                String::from_utf8_lossy(&result.stdout)
            )
        });
    tokio::spawn(connection);
    client
        .batch_execute(&format!("DROP SCHEMA IF EXISTS \"{namespace}\" CASCADE"))
        .await
        .unwrap();
    assert!(
        result.status.success(),
        "Service integration under {fallback} failed: {}\n{}",
        String::from_utf8_lossy(&result.stderr),
        String::from_utf8_lossy(&result.stdout)
    );
}
