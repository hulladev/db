#![cfg(feature = "typescript")]
use serde_json::{Value, json};
use std::process::Stdio;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};
use uuid::Uuid;
struct Bridge {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}
fn url() -> String {
    std::env::var("HULLADB_TEST_URL").expect("set HULLADB_TEST_URL")
}
fn descriptor() -> Value {
    json!({"uuid_default":"V7","tables":[{"name":"tasks","key":"id","generation":"UuidV7","insert_policy":"AllowExplicit","fields":[{"name":"title","scalar":"String","nullable":false,"default":null,"constraints":{"oneOf":[]}},{"name":"id","scalar":"Uuid","nullable":false,"default":null,"constraints":{"oneOf":[]}}],"indexes":[],"checks":[],"unique":[{"name":"unique_title","fields":["title"],"nulls":"NotDistinct"}]}],"references":[]})
}
impl Bridge {
    async fn start(namespace: &str, initialize: bool) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_hulladb-bridge"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut bridge = Self {
            child,
            input,
            output,
        };
        let response = bridge.call(json!({"type":"init","url":url(),"namespace":namespace,"schema":descriptor(),"initialize":initialize})).await;
        assert_eq!(response["type"], "ready", "{response}");
        bridge
    }
    async fn call(&mut self, value: Value) -> Value {
        self.input
            .write_all(format!("{value}\n").as_bytes())
            .await
            .unwrap();
        self.input.flush().await.unwrap();
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            self.output.read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        serde_json::from_str(&line).unwrap_or_else(|_| panic!("invalid bridge response {line:?}"))
    }
    async fn begin(&mut self, mode: &str) {
        assert_eq!(
            self.call(json!({"type":"begin","mode":mode})).await["type"],
            "begun"
        );
    }
    async fn operation(&mut self, operation: Value) -> Value {
        self.call(json!({"type":"operation","id":1,"operation":operation}))
            .await
    }
    async fn commit(&mut self) -> Value {
        self.call(json!({"type":"commit"})).await
    }
    async fn close(self) {
        let Self {
            mut child, input, ..
        } = self;
        drop(input);
        assert!(child.wait().await.unwrap().success());
    }
}
async fn inspect(namespace: &str) -> (i64, i64, i64) {
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(connection);
    let row = client.query_one(&format!("SELECT (SELECT sequence FROM \"{namespace}\".scopes), (SELECT count(*) FROM \"{namespace}\".t0), (SELECT count(*) FROM \"{namespace}\".receipts)"),&[]).await.unwrap();
    (row.get(0), row.get(1), row.get(2))
}
async fn cleanup(namespace: &str) {
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(connection);
    client
        .batch_execute(&format!("DROP SCHEMA \"{namespace}\" CASCADE"))
        .await
        .unwrap();
}
#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn direct_transactions_share_revisions_without_receipts_and_rollback_failures() {
    let namespace = format!("bridge_{}", Uuid::now_v7().simple());
    let mut bridge = Bridge::start(&namespace, true).await;
    bridge.begin("write").await;
    let rows: Vec<_> = (0..1001)
        .map(|i| json!({"title":format!("task{i}")}))
        .collect();
    let result = bridge
        .operation(json!({"method":"insert","table":"tasks","values":rows}))
        .await;
    assert_eq!(result["result"].as_array().unwrap().len(), 1001);
    assert_eq!(bridge.commit().await["type"], "committed");
    assert_eq!(inspect(&namespace).await, (1, 1001, 0));
    // Re-running explicit installation verifies the same schema and preserves data.
    Bridge::start(&namespace, true).await.close().await;
    assert_eq!(inspect(&namespace).await, (1, 1001, 0));

    bridge.begin("read").await;
    let rows = bridge
        .operation(json!({"method":"select","table":"tasks","filters":[],"limit":null}))
        .await;
    assert_eq!(
        rows["result"].as_array().unwrap().len(),
        1001,
        "unbounded select must not silently truncate at1000"
    );
    assert_eq!(bridge.commit().await["type"], "committed");
    bridge.begin("write").await;
    let contradiction = json!([{"kind":"compare","field":"title","operator":"=","operand":{"kind":"value","value":"task1"}},{"kind":"compare","field":"title","operator":"=","operand":{"kind":"value","value":"task2"}}]);
    assert_eq!(bridge.operation(json!({"method":"update","table":"tasks","filters":contradiction,"values":{"title":{"kind":"value","value":"never"}}})).await["result"],json!([]));
    assert_eq!(bridge.operation(json!({"method":"delete","table":"tasks","filters":[{"kind":"compare","field":"title","operator":"=","operand":{"kind":"value","value":"missing"}}]})).await["result"],json!([]));
    assert_eq!(bridge.commit().await["type"], "committed");
    assert_eq!(inspect(&namespace).await, (1, 1001, 0));
    bridge.begin("write").await;
    bridge
        .operation(json!({"method":"insert","table":"tasks","values":{"title":"rollback"}}))
        .await;
    assert_eq!(
        bridge.call(json!({"type":"rollback"})).await["type"],
        "rolled_back"
    );
    bridge.begin("write").await;
    let failure=bridge.operation(json!({"method":"insert","table":"tasks","values":[{"title":"partial"},{"title":"task1"}]})).await;
    assert_eq!(failure["error"]["code"], "constraint_violation");
    assert_eq!(failure["rolledBack"], true);
    assert_eq!(inspect(&namespace).await, (1, 1001, 0));
    bridge.begin("read").await;
    assert_eq!(
        bridge
            .operation(json!({"method":"insert","table":"tasks","values":{"title":"forbidden"}}))
            .await["error"]["code"],
        "read_only"
    );
    bridge.begin("write").await;
    bridge.operation(json!({"method":"delete","table":"tasks","filters":[{"kind":"compare","field":"title","operator":"=","operand":{"kind":"value","value":"task1"}}]})).await;
    bridge.close().await; // stdin EOF rolls back the open transaction.
    assert_eq!(inspect(&namespace).await, (1, 1001, 0));
    cleanup(&namespace).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn direct_serialization_conflicts_do_not_replay_server_work() {
    let namespace = format!("bridge_{}", Uuid::now_v7().simple());
    let mut first = Bridge::start(&namespace, true).await;
    let mut second = Bridge::start(&namespace, false).await;
    first.begin("write").await;
    second.begin("write").await;
    first
        .operation(json!({"method":"select","table":"tasks","filters":[],"limit":null}))
        .await;
    second
        .operation(json!({"method":"insert","table":"tasks","values":{"title":"winner"}}))
        .await;
    assert_eq!(second.commit().await["type"], "committed");
    let insert = first
        .operation(json!({"method":"insert","table":"tasks","values":{"title":"must-not-replay"}}))
        .await;
    let failure = if insert["type"] == "error" {
        insert
    } else {
        first.commit().await
    };
    assert_eq!(failure["error"]["code"], "serialization_conflict");
    assert_eq!(failure["rolledBack"], true);
    assert_eq!(inspect(&namespace).await, (1, 1, 0));
    first.begin("read").await;
    assert_eq!(
        first
            .operation(json!({"method":"select","table":"tasks","filters":[],"limit":null}))
            .await["result"][0]["row"]["title"],
        "winner"
    );
    assert_eq!(first.commit().await["type"], "committed");
    first.close().await;
    second.close().await;
    cleanup(&namespace).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL and Bun"]
async fn generated_server_database_executes_real_transactions() {
    generated_server_database("BUN", "bun", "tests/direct-server.integration.ts").await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL, Node.js and Bun for generation"]
async fn generated_server_database_executes_real_transactions_under_node() {
    generated_server_database("NODE", "node", "tests/direct-server.integration.ts").await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL, Bun and Better Auth workspace dependencies"]
async fn better_auth_storage_under_bun() {
    generated_server_database("BUN", "bun", "../better-auth/tests/adapter.integration.ts").await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL, Node.js and Better Auth workspace dependencies"]
async fn better_auth_storage_under_node() {
    generated_server_database(
        "NODE",
        "node",
        "../better-auth/tests/adapter.integration.ts",
    )
    .await;
}

async fn generated_server_database(runtime_variable: &str, fallback: &str, fixture: &str) {
    let namespace = format!("bridge_{}", Uuid::now_v7().simple());
    let package = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/db");
    let result = Command::new(std::env::var(runtime_variable).unwrap_or_else(|_| fallback.into()))
        .arg("--conditions=hulladb-source")
        .arg(fixture)
        .current_dir(package)
        .env("HULLADB_TEST_URL", url())
        .env("HULLADB_TEST_NAMESPACE", &namespace)
        .env("HULLADB_ENGINE", env!("CARGO_BIN_EXE_hulladb-bridge"))
        .kill_on_drop(true)
        .output()
        .await
        .expect("launch runtime integration fixture");
    // The script owns explicit creation; remove only this test's namespace,
    // including after failure before initialization completes.
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
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
        "TypeScript integration under {fallback} failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[tokio::test]
#[ignore = "requires PostgreSQL; exercises the real 30-second suspended callback bound"]
async fn suspended_callback_releases_postgres_transaction_without_host_commands() {
    let namespace = format!("bridge_{}", Uuid::now_v7().simple());
    let mut bridge = Bridge::start(&namespace, true).await;
    assert_eq!(bridge.call(json!({"type":"ping"})).await["type"], "pong");
    let mut idle = Bridge::start(&namespace, false).await;
    bridge.begin("write").await;
    let inserted = bridge
        .operation(json!({"method":"insert","table":"tasks","values":{"title":"suspended"}}))
        .await;
    assert_eq!(inserted["type"], "operation_result");
    // No JS/native command is sent while PostgreSQL enforces the idle bound.
    tokio::time::sleep(std::time::Duration::from_secs(32)).await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(3), idle.child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    let mut replacement = Bridge::start(&namespace, false).await;
    replacement.begin("write").await;
    assert_eq!(
        replacement
            .operation(json!({"method":"select","table":"tasks","filters":[],"limit":null}))
            .await["result"],
        json!([])
    );
    assert_eq!(replacement.commit().await["type"], "committed");
    // The expired callback cannot retain a native process or revive its transaction.
    let mut expiration = String::new();
    bridge.output.read_line(&mut expiration).await.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&expiration).unwrap()["type"],
        "error"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(3), bridge.child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    replacement.close().await;
    cleanup(&namespace).await;
}
