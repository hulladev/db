//! Real request timings against the local PostgreSQL development probe.
use std::{hint::black_box, sync::Arc, time::Instant};

use hulladb_postgres::{
    Command, QueryCache, QueryRevision, QuerySnapshot, Session, Store, TaskQuery, TaskSummary, Uuid,
};
use serde_json::{Value, json};
use tokio::{sync::Barrier, task::JoinSet};

const SAMPLES: usize = 31;
const WARMUP: usize = 5;

enum Work {
    List {
        completed: Option<bool>,
        after: Option<Uuid>,
    },
    Create,
    Patch {
        id: Uuid,
        version: i64,
    },
    Replay {
        request: Uuid,
        command: Command,
    },
    Query(TaskQuery),
}

fn median(samples: &[f64]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

fn result(name: &str, workload: Value, samples: Vec<f64>) -> Value {
    let center = median(&samples);
    let deviations: Vec<_> = samples
        .iter()
        .map(|sample| (sample - center).abs())
        .collect();
    let mut sorted = samples.clone();
    sorted.sort_by(f64::total_cmp);
    json!({
        "name": name, "fixture_version": 2, "workload": workload,
        "sample_kind": "request_latency", "iterations_per_sample": 1,
        "median_ns": center, "mad_ns": median(&deviations), "samples_ns": samples,
        "p95_ns": sorted[(95 * sorted.len()).div_ceil(100) - 1],
    })
}

async fn measure(
    store: &mut Store,
    session: Session,
    name: &str,
    workload: Value,
    mut work: Work,
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut samples = Vec::new();
    for index in 0..SAMPLES + WARMUP {
        let request = Uuid::now_v7();
        let command = match &work {
            Work::Create => Some(Command::Create {
                id: Uuid::now_v7(),
                title: "benchmark task".into(),
                completed: false,
            }),
            Work::Patch { id, version } => Some(Command::Patch {
                id: *id,
                expected_version: *version,
                title: None,
                completed: Some(index.is_multiple_of(2)),
            }),
            Work::Replay { command, .. } => Some(command.clone()),
            _ => None,
        };
        let start = Instant::now();
        match &mut work {
            Work::List { completed, after } => {
                black_box(store.list_tasks(session, *completed, *after, 50).await?);
            }
            Work::Create => {
                black_box(store.execute(session, request, command.unwrap()).await?);
            }
            Work::Patch { version, .. } => {
                black_box(store.execute(session, request, command.unwrap()).await?);
                *version += 1;
            }
            Work::Replay { request, .. } => {
                black_box(store.execute(session, *request, command.unwrap()).await?);
            }
            Work::Query(query) => {
                black_box(store.query_snapshot(session, *query).await?);
            }
        }
        if index >= WARMUP {
            samples.push(start.elapsed().as_nanos() as f64);
        }
    }
    Ok(result(name, workload, samples))
}

async fn contention(
    url: &str,
    admin: &mut Store,
    shared: bool,
) -> Result<Value, Box<dyn std::error::Error>> {
    let first = admin.create_workspace(Uuid::now_v7()).await?;
    let mut fixtures = vec![first.workspace];
    let mut workers = Vec::new();
    for index in 0..4 {
        let session = if shared || index == 0 {
            first
        } else {
            let session = admin.create_workspace(Uuid::now_v7()).await?;
            fixtures.push(session.workspace);
            session
        };
        workers.push((Store::connect(url).await?, session));
    }
    let barrier = Arc::new(Barrier::new(4));
    let mut tasks = JoinSet::new();
    let start = Instant::now();
    for (mut store, session) in workers {
        let barrier = barrier.clone();
        tasks.spawn(async move {
            let mut samples = Vec::new();
            for _ in 0..SAMPLES {
                let request = Uuid::now_v7();
                let command = Command::Create {
                    id: Uuid::now_v7(),
                    title: "concurrent task".into(),
                    completed: false,
                };
                barrier.wait().await;
                let start = Instant::now();
                store.execute(session, request, command).await?;
                samples.push(start.elapsed().as_nanos() as f64);
            }
            Ok::<_, hulladb_postgres::Error>(samples)
        });
    }
    let mut samples = Vec::new();
    while let Some(worker) = tasks.join_next().await {
        samples.extend(worker??);
    }
    let elapsed = start.elapsed().as_secs_f64();
    let count = samples.len();
    let mut report = result(
        if shared {
            "create_contended/one_workspace"
        } else {
            "create_contended/four_workspaces"
        },
        json!({"writers": 4, "requests_per_writer": SAMPLES, "workspaces": if shared { 1 } else { 4 }}),
        samples,
    );
    report["throughput_ops_per_second"] = json!(count as f64 / elapsed);
    for workspace in fixtures {
        admin.delete_fixture_workspace(workspace).await?;
    }
    Ok(report)
}

// Cached-result access has no database or allocation work. Batch these readings
// rather than presenting clock overhead as individual-request tail latency.
fn cached_query() -> Value {
    let session = Session {
        workspace: Uuid::now_v7(),
        principal: Uuid::now_v7(),
    };
    let query = TaskQuery {
        completed: Some(false),
        after: None,
        limit: 50,
    };
    let mut cache = QueryCache::new(session, query).unwrap();
    cache
        .install(QuerySnapshot {
            session,
            query,
            revision: QueryRevision {
                workspace: session.workspace,
                generation: 2,
                sequence: 0,
            },
            tasks: (1..=50)
                .map(|id| TaskSummary {
                    id: Uuid::from_u128(id),
                    title: "cached task".into(),
                    completed: false,
                })
                .collect(),
        })
        .unwrap();
    const ITERATIONS: usize = 100_000;
    let batch = || {
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            black_box(cache.read(black_box(session), black_box(query)).unwrap());
        }
        start.elapsed().as_nanos() as f64 / ITERATIONS as f64
    };
    let warmup = Instant::now();
    while warmup.elapsed().as_millis() < 50 {
        black_box(batch());
    }
    let samples = (0..SAMPLES).map(|_| batch()).collect();
    let mut report = result(
        "query_cache_read/50",
        json!({"cached_rows": 50, "projection": ["id", "title", "completed"], "borrowed_result": true}),
        samples,
    );
    report["sample_kind"] = json!("batch_average");
    report["iterations_per_sample"] = json!(ITERATIONS);
    report.as_object_mut().unwrap().remove("p95_ns");
    report
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !std::env::args().any(|arg| arg == "--bench") {
        println!("PostgreSQL timings require an explicit benchmark run; use make db-bench.");
        return Ok(());
    }
    let url = std::env::var("HULLADB_TEST_URL")
        .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into());
    let mut store = Store::connect(&url).await?;
    store.initialize().await?;
    let mut results = Vec::new();
    let mut plans = Vec::new();
    let mut version = String::new();
    let mut settings = json!(null);
    for rows in [1_000, 100_000] {
        let session = store.create_workspace(Uuid::now_v7()).await?;
        store.seed_fixture(session, rows).await?;
        let diagnostics = store.fixture_diagnostics(session.workspace).await?;
        version = diagnostics["server_version"].as_str().unwrap().into();
        settings = diagnostics["settings"].clone();
        plans.push(json!({"rows": rows, "plan": diagnostics["status_keyset_plan"]}));
        results.push(
            measure(
                &mut store,
                session,
                &format!("list_tasks/{rows}"),
                json!({"rows": rows, "limit": 50, "completed": false}),
                Work::List {
                    completed: Some(false),
                    after: None,
                },
            )
            .await?,
        );
        results.push(measure(&mut store, session, &format!("keyset_page/{rows}"), json!({"rows": rows, "limit": 50, "completed": false, "after": "80000000-0000-0000-0000-000000000000"}), Work::List { completed: Some(false), after: Some(Uuid::from_u128(1 << 127)) }).await?);
        results.push(measure(&mut store, session, &format!("query_snapshot/{rows}"), json!({"rows": rows, "limit": 50, "completed": false, "projection": ["id", "title", "completed"], "complete_at_revision": true}), Work::Query(TaskQuery { completed: Some(false), after: None, limit: 50 })).await?);
        store.delete_fixture_workspace(session.workspace).await?;
    }
    let session = store.create_workspace(Uuid::now_v7()).await?;
    results.push(
        measure(
            &mut store,
            session,
            "command_create",
            json!({"fields": 4, "title_bytes": 14}),
            Work::Create,
        )
        .await?,
    );
    let id = Uuid::now_v7();
    let request = Uuid::now_v7();
    let command = Command::Create {
        id,
        title: "replay task".into(),
        completed: false,
    };
    store.execute(session, request, command.clone()).await?;
    results.push(
        measure(
            &mut store,
            session,
            "command_replay",
            json!({"existing_receipt": true}),
            Work::Replay { request, command },
        )
        .await?,
    );
    results.push(
        measure(
            &mut store,
            session,
            "command_patch",
            json!({"changed_fields": 1, "expected_version": true}),
            Work::Patch { id, version: 1 },
        )
        .await?,
    );
    store.delete_fixture_workspace(session.workspace).await?;
    results.push(contention(&url, &mut store, true).await?);
    results.push(contention(&url, &mut store, false).await?);
    results.push(cached_query());
    let config: tokio_postgres::Config = url.parse()?;
    println!(
        "{}",
        json!({"format_version": 1, "results": results, "query_plans": plans,
        "database": {"server_version": version, "settings": settings, "hosts": format!("{:?}", config.get_hosts()), "ports": config.get_ports(), "database": config.get_dbname()}})
    );
    Ok(())
}
