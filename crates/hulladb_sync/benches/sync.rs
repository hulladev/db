//! Managed commands, durable revision polling, authorized delivery and client SQLite.
#[path = "../tests/support/commands.rs"]
mod commands;

use std::{hint::black_box, time::Instant};

use commands::CommandFixture;
use hulladb_client::{ReplicaOptions, TaskReplica};
use hulladb_postgres::{Command, TaskQuery, Uuid};
use hulladb_sync::TaskFeed;
use serde_json::{Value as Json, json};

const SAMPLES: usize = 31;
const WARMUP: usize = 5;

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[sorted.len() / 2]
}
fn result(name: &str, rows: i32, samples: Vec<f64>) -> Json {
    let center = median(&samples);
    let deviations = samples
        .iter()
        .map(|v| (v - center).abs())
        .collect::<Vec<_>>();
    let mut sorted = samples.clone();
    sorted.sort_by(f64::total_cmp);
    json!({"name":format!("{name}/{rows}"),"fixture_version":6,
        "workload":{"workspace_rows":rows,"storage":"SQLite WAL/FULL","invalidation":"durable managed revision polling, immediate refresh after commit","source":"actual command table","bootstrap":"authorized query into client SQLite","projection":"id/title/completed","limit":50},
        "sample_kind":"request_latency","iterations_per_sample":1,
        "median_ns":center,"mad_ns":median(&deviations),"p95_ns":sorted[(95*sorted.len()).div_ceil(100)-1],"samples_ns":samples})
}
fn options() -> ReplicaOptions {
    ReplicaOptions {
        max_chunk_rows: 32,
        max_chunk_bytes: 64 * 1024,
        retained_deliveries_per_query: 2,
        busy_timeout: std::time::Duration::from_secs(1),
    }
}
fn clear(path: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !std::env::args().any(|arg| arg == "--bench") {
        println!("Use make sync-bench to measure the managed transaction execution path.");
        return Ok(());
    }
    let mut results = Vec::new();
    let mut database = json!(null);
    for rows in [1, 512] {
        let mut f = CommandFixture::create(rows).await?;
        let diagnostics = f.store.fixture_diagnostics(f.session.workspace).await?;
        let config: tokio_postgres::Config = f.url.parse()?;
        database = json!({"server_version":diagnostics["server_version"],"settings":diagnostics["settings"],"hosts":format!("{:?}",config.get_hosts()),"ports":config.get_ports(),"database":config.get_dbname()});
        let query = TaskQuery {
            completed: None,
            after: None,
            limit: 50,
        };
        let mut boot = Vec::new();
        for sample in 0..SAMPLES + WARMUP {
            let start = Instant::now();
            let mut feed = TaskFeed::new(&f.store, 1).await?;
            let mut subscription = feed.subscribe(&mut f.store, f.session, query).await?;
            let initial = subscription.next(&f.store).await?;
            let mut client = TaskReplica::open(&f.client_path, f.session, options())?;
            client.register(query, initial.cursor.subscription)?;
            client.apply(&initial)?;
            let elapsed = start.elapsed().as_nanos() as f64;
            assert_eq!(
                client.read(f.session, query)?.tasks.len(),
                (rows as usize).min(50)
            );
            if sample >= WARMUP {
                boot.push(elapsed);
            }
            drop(client);
            clear(&f.client_path);
        }
        results.push(result("bootstrap", rows, boot));
        let mut feed = TaskFeed::new(&f.store, 1).await?;
        let mut subscription = feed.subscribe(&mut f.store, f.session, query).await?;
        let initial = subscription.next(&f.store).await?;
        let mut client = TaskReplica::open(&f.client_path, f.session, options())?;
        client.register(query, initial.cursor.subscription)?;
        client.apply(&initial)?;
        let id = f.store.list_tasks(f.session, None, None, 1).await?.tasks[0].id;
        let initial_completed = client
            .read(f.session, query)?
            .tasks
            .iter()
            .find(|task| task.id == id)
            .unwrap()
            .completed;
        let mut commit = Vec::new();
        let mut visibility = Vec::new();
        let mut total = Vec::new();
        let mut reads = Vec::new();
        for sample in 0..SAMPLES + WARMUP {
            let command = Command::Patch {
                id,
                expected_version: sample as i64 + 1,
                title: None,
                completed: Some(if sample.is_multiple_of(2) {
                    !initial_completed
                } else {
                    initial_completed
                }),
            };
            let request = Uuid::now_v7();
            let start = Instant::now();
            black_box(f.store.execute(f.session, request, command).await?);
            let acknowledged = Instant::now();
            feed.refresh(&mut f.store).await?;
            client.apply(subscription.next(&f.store).await?.as_ref())?;
            let visible = Instant::now();
            let view = client.read(f.session, query)?;
            let read_ns = visible.elapsed().as_nanos() as f64;
            let updated = view.tasks.iter().find(|task| task.id == id).unwrap();
            assert_eq!(
                updated.completed,
                if sample.is_multiple_of(2) {
                    !initial_completed
                } else {
                    initial_completed
                }
            );
            if sample >= WARMUP {
                commit.push(acknowledged.duration_since(start).as_nanos() as f64);
                visibility.push(visible.duration_since(acknowledged).as_nanos() as f64);
                total.push(visible.duration_since(start).as_nanos() as f64);
                reads.push(read_ns);
            }
        }
        results.push(result("command_commit", rows, commit));
        results.push(result("commit_to_client", rows, visibility));
        results.push(result("command_to_client", rows, total));
        results.push(result("client_query", rows, reads));
        drop(client);
        f.cleanup().await?;
    }
    println!(
        "{}",
        json!({"format_version":1,"results":results,"database":database})
    );
    Ok(())
}
