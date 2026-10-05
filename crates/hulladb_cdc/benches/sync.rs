//! Sole CDC path: actual commands, WAL capture, durable log and durable replica.
#[path = "../tests/support/commands.rs"]
mod commands;

use std::{hint::black_box, time::Instant};

use commands::CommandFixture;
use hulladb_cdc::{Capture, EventLog, Replica, Retention, Value};
use hulladb_postgres::{Command, Uuid};
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
    json!({"name":format!("{name}/{rows}"),"fixture_version":3,
        "workload":{"workspace_rows":rows,"storage":"SQLite WAL/FULL","capture":"pgoutput persistent stream","source":"actual command table","bootstrap":"slot creation and exported snapshot","projection":"full rows"},
        "sample_kind":"request_latency","iterations_per_sample":1,
        "median_ns":center,"mad_ns":median(&deviations),"p95_ns":sorted[(95*sorted.len()).div_ceil(100)-1],"samples_ns":samples})
}
fn clear(path: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !std::env::args().any(|arg| arg == "--bench") {
        println!("Use make sync-bench to measure the CDC execution path.");
        return Ok(());
    }
    let mut results = Vec::new();
    let mut database = json!(null);
    for rows in [1, 512] {
        let mut f = CommandFixture::create(rows).await?;
        let diagnostics = f.store.fixture_diagnostics(f.session.workspace).await?;
        let config: tokio_postgres::Config = f.url.parse()?;
        database = json!({"server_version":diagnostics["server_version"],"settings":diagnostics["settings"],"hosts":format!("{:?}",config.get_hosts()),"ports":config.get_ports(),"database":config.get_dbname()});
        let mut boot = Vec::new();
        for sample in 0..SAMPLES + WARMUP {
            let start = Instant::now();
            let snapshot = f.bootstrap().await?.read().await?;
            let replica = Replica::initialize(&f.replica_path, snapshot)?;
            let elapsed = start.elapsed().as_nanos() as f64;
            let view = replica.view(f.table, 1000)?;
            assert!(view.complete);
            assert_eq!(
                view.rows
                    .iter()
                    .filter(|r| r[0] == Value::Text(f.session.workspace.to_string()))
                    .count(),
                rows as usize
            );
            if sample >= WARMUP {
                boot.push(elapsed);
            }
            drop(replica);
            f.db.query_one("SELECT pg_drop_replication_slot($1)", &[&f.source.slot])
                .await?;
            clear(&f.replica_path);
        }
        results.push(result("bootstrap", rows, boot));
        let snapshot = f.bootstrap().await?.read().await?;
        let mut replica = Replica::initialize(&f.replica_path, snapshot)?;
        let mut log = EventLog::open(&f.log_path, f.source.clone(), Retention::default())?;
        let mut capture = Capture::connect(&f.url, &f.source).await?;
        let id = f.store.list_tasks(f.session, None, None, 1).await?.tasks[0].id;
        let mut commit = Vec::new();
        let mut visibility = Vec::new();
        let mut total = Vec::new();
        let mut reads = Vec::new();
        for sample in 0..SAMPLES + WARMUP {
            let command = Command::Patch {
                id,
                expected_version: sample as i64 + 1,
                title: None,
                completed: Some(sample.is_multiple_of(2)),
            };
            let request = Uuid::now_v7();
            let start = Instant::now();
            black_box(f.store.execute(f.session, request, command).await?);
            let acknowledged = Instant::now();
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            while log.head()?.sequence < sample as i64 + 1 {
                tokio::time::timeout_at(deadline, capture.next(&mut log)).await??;
                assert!(tokio::time::Instant::now() < deadline, "capture stalled");
                tokio::task::yield_now().await;
            }
            replica.catch_up(&log, 100)?;
            let visible = Instant::now();
            let view = replica.view(f.table, 1000)?;
            let read_ns = visible.elapsed().as_nanos() as f64;
            assert!(view.complete);
            assert_eq!(view.cursor, log.head()?);
            let updated = view
                .rows
                .iter()
                .find(|r| {
                    r[0] == Value::Text(f.session.workspace.to_string())
                        && r[1] == Value::Text(id.to_string())
                })
                .unwrap();
            assert_eq!(updated[4], Value::Text((sample + 2).to_string()));
            if sample >= WARMUP {
                commit.push(acknowledged.duration_since(start).as_nanos() as f64);
                visibility.push(visible.duration_since(acknowledged).as_nanos() as f64);
                total.push(visible.duration_since(start).as_nanos() as f64);
                reads.push(read_ns);
            }
        }
        results.push(result("command_commit", rows, commit));
        results.push(result("commit_to_replica", rows, visibility));
        results.push(result("command_to_replica", rows, total));
        results.push(result("replica_view", rows, reads));
        capture.close().await?;
        drop(replica);
        drop(log);
        f.cleanup().await?;
    }
    println!(
        "{}",
        json!({"format_version":1,"results":results,"database":database})
    );
    Ok(())
}
