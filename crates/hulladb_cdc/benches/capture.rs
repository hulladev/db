//! Warm local CDC/storage baseline. Capture, bounded snapshot copy, and durable replica application.
#[path = "../tests/support/mod.rs"]
mod support;

use std::{hint::black_box, time::Instant};

use hulladb_cdc::{Capture, Cursor, EventLog, Replica, Retention};
use serde_json::{Value, json};
use support::Fixture;

const SAMPLES: usize = 31;
const WARMUP: usize = 5;

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[sorted.len() / 2]
}
fn result(name: &str, workload: Value, samples: Vec<f64>) -> Value {
    let center = median(&samples);
    let deviations: Vec<_> = samples.iter().map(|value| (value - center).abs()).collect();
    let mut sorted = samples.clone();
    sorted.sort_by(f64::total_cmp);
    json!({"name": name, "fixture_version": 2, "workload": workload,
        "sample_kind": "request_latency", "iterations_per_sample": 1,
        "median_ns": center, "mad_ns": median(&deviations), "samples_ns": samples,
        "p95_ns": sorted[(95 * sorted.len()).div_ceil(100) - 1]})
}

async fn capture_one(
    capture: &mut Capture,
    log: &mut EventLog,
) -> Result<hulladb_cdc::Progress, Box<dyn std::error::Error>> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let report = tokio::time::timeout_at(deadline, capture.next(log)).await??;
        if !report.appended.is_empty() {
            return Ok(report);
        }
    }
}

async fn capture_case(
    name: &str,
    rows: i64,
    include_write: bool,
) -> Result<Value, Box<dyn std::error::Error>> {
    let f = Fixture::create().await?;
    let mut log = EventLog::open(&f.path, f.source.clone(), Retention::default())?;
    let mut capture = Capture::connect(&f.url, &f.source).await?;
    let mut samples = Vec::new();
    for index in 0..SAMPLES + WARMUP {
        let low = (index as i64) * rows;
        let sql = format!(
            "INSERT INTO {}.tasks SELECT i,$3,false FROM generate_series($1::bigint,$2::bigint) i",
            f.schema
        );
        let title = "benchmark task";
        if !include_write {
            f.db.execute(&sql, &[&(low + 1), &(low + rows), &title])
                .await?;
        }
        let start = Instant::now();
        if include_write {
            f.db.execute(&sql, &[&(low + 1), &(low + rows), &title])
                .await?;
        }
        let report = capture_one(&mut capture, &mut log).await?;
        let elapsed = start.elapsed().as_nanos() as f64;
        assert_eq!(report.appended.len(), 1);
        black_box(report);
        if index >= WARMUP {
            samples.push(elapsed);
        }
    }
    let report = result(
        name,
        json!({"rows_per_transaction": rows, "title_bytes": 14, "sql_commit_included": include_write, "sqlite_synchronous": "FULL", "transport": "pgoutput_stream", "retention_batches": 128, "retention_bytes": 16777216}),
        samples,
    );
    capture.close().await?;
    drop(log);
    f.cleanup().await?;
    Ok(report)
}

async fn reads_and_growth() -> Result<(Vec<Value>, Vec<Value>, Value), Box<dyn std::error::Error>> {
    let f = Fixture::create().await?;
    let mut log = EventLog::open(&f.path, f.source.clone(), Retention::default())?;
    let mut capture = Capture::connect(&f.url, &f.source).await?;
    let version: String = f.db.query_one("SHOW server_version", &[]).await?.get(0);
    let sqlite_version: String =
        rusqlite::Connection::open(&f.path)?
            .query_row("SELECT sqlite_version()", [], |r| r.get(0))?;
    let config: tokio_postgres::Config = f.url.parse()?;
    let settings: Value = f.db.query_one("SELECT json_build_object('wal_level',current_setting('wal_level'),'synchronous_commit',current_setting('synchronous_commit'),'fsync',current_setting('fsync'),'full_page_writes',current_setting('full_page_writes'),'max_slot_wal_keep_size',current_setting('max_slot_wal_keep_size'))", &[]).await?.get(0);
    let database = json!({"server_version": version, "hosts": format!("{:?}", config.get_hosts()), "ports": config.get_ports(), "database": config.get_dbname(), "settings": settings, "sqlite_version": sqlite_version, "sqlite_synchronous": "FULL"});
    let mut growth = Vec::new();
    for id in 1_i64..=512 {
        f.db.execute(
            &format!(
                "INSERT INTO {}.tasks VALUES($1,'benchmark task',false)",
                f.schema
            ),
            &[&id],
        )
        .await?;
        assert_eq!(capture_one(&mut capture, &mut log).await?.appended.len(), 1);
        if [64, 128, 256, 512].contains(&id) {
            let s = log.stats()?;
            let wal_file_bytes =
                std::fs::metadata(format!("{}-wal", f.path.display())).map_or(0, |m| m.len());
            let postgres_table_bytes: i64 =
                f.db.query_one(
                    "SELECT pg_total_relation_size($1::text::regclass)",
                    &[&format!("{}.tasks", f.schema)],
                )
                .await?
                .get(0);
            growth.push(json!({"committed_transactions": id, "retained_batches": s.batches, "retained_payload_bytes": s.payload_bytes, "sqlite_database_page_bytes": s.database_bytes, "sqlite_reusable_page_bytes": s.reusable_bytes, "sqlite_wal_file_bytes": wal_file_bytes, "postgres_business_table_bytes": postgres_table_bytes}));
        }
    }
    let cursor = Cursor {
        generation: f.source.generation,
        sequence: log.head()?.sequence - 100,
    };
    let mut samples = Vec::new();
    for index in 0..SAMPLES + WARMUP {
        let start = Instant::now();
        let events = log.since(cursor, 100)?;
        let elapsed = start.elapsed().as_nanos() as f64;
        assert_eq!(events.len(), 100);
        black_box(events);
        if index >= WARMUP {
            samples.push(elapsed);
        }
    }
    let read = result(
        "cdc_log_read/100",
        json!({"batches": 100, "rows_per_batch": 1, "json_decoding_included": true, "sqlite": "local_file"}),
        samples,
    );
    capture.close().await?;
    drop(log);
    f.cleanup().await?;
    Ok((vec![read], growth, database))
}

async fn snapshot_and_replica() -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let mut f = Fixture::create_for_bootstrap(false).await?;
    f.db.batch_execute(&format!(
        "INSERT INTO {}.tasks SELECT i,'benchmark task',false FROM generate_series(1,1000) i",
        f.schema
    ))
    .await?;
    let replica_path = f.path.with_extension("replica.sqlite");
    let mut samples = Vec::new();
    for index in 0..SAMPLES + WARMUP {
        // Connection establishment, snapshot acquisition, and locks are excluded.
        let reader = f.bootstrap().await?;
        let start = Instant::now();
        let replica = Replica::initialize(&replica_path, reader.read().await?)?;
        let elapsed = start.elapsed().as_nanos() as f64;
        black_box(&replica);
        drop(replica);
        f.db.query_one("SELECT pg_drop_replication_slot($1)", &[&f.source.slot])
            .await?;
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", replica_path.display()));
        }
        if index >= WARMUP {
            samples.push(elapsed);
        }
    }
    let mut copy = result(
        "cdc_snapshot_copy/1000",
        json!({"rows":1000,"title_bytes":14,"sqlite_synchronous":"FULL","includes":"metadata_and_copy_and_sqlite_initialization","connection_and_snapshot_acquisition_included":false}),
        samples,
    );
    copy["fixture_version"] = json!(3);
    copy["workload"]["bootstrap"] = json!("slot-exported snapshot");
    let snapshot = f.bootstrap().await?.read().await?;
    let mut replica = Replica::initialize(&replica_path, snapshot)?;
    let mut log = EventLog::open(&f.path, f.source.clone(), Retention::default())?;
    let mut capture = Capture::connect(&f.url, &f.source).await?;
    let mut samples = Vec::new();
    for index in 0..SAMPLES + WARMUP {
        f.db.batch_execute(&format!(
            "UPDATE {}.tasks SET title='updated task' WHERE id<=100",
            f.schema
        ))
        .await?;
        assert_eq!(capture_one(&mut capture, &mut log).await?.appended.len(), 1);
        let start = Instant::now();
        assert_eq!(replica.catch_up(&log, 100)?, 1);
        let elapsed = start.elapsed().as_nanos() as f64;
        assert_eq!(replica.cursor()?, log.head()?);
        if index >= WARMUP {
            samples.push(elapsed);
        }
    }
    let mut apply = result(
        "cdc_replica_apply/100",
        json!({"rows_per_transaction":100,"materialized_rows":1000,"sqlite_synchronous":"FULL","includes":"log_read_decode_and_atomic_replica_apply_and_size_check","sql_write_and_capture_included":false}),
        samples,
    );
    apply["fixture_version"] = json!(3);
    apply["workload"]["bootstrap"] = json!("slot-exported snapshot");
    capture.close().await?;
    drop(replica);
    drop(log);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", replica_path.display()));
    }
    f.cleanup().await?;
    Ok(vec![copy, apply])
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !std::env::args().any(|arg| arg == "--bench") {
        println!("CDC timings require an explicit run; use make cdc-bench.");
        return Ok(());
    }
    let mut results = Vec::new();
    results.push(capture_case("cdc_capture/1", 1, false).await?);
    results.push(capture_case("cdc_commit_and_capture/1", 1, true).await?);
    results.push(capture_case("cdc_capture/100", 100, false).await?);
    let (read_results, growth, database) = reads_and_growth().await?;
    results.extend(read_results);
    results.extend(snapshot_and_replica().await?);
    println!(
        "{}",
        json!({"format_version": 1, "results": results, "storage_growth": growth, "database": database})
    );
    Ok(())
}
