# Performance workloads and baselines

Measure the retained execution model. Schema/codec operations, authoritative
commands/queries and CDC stages have separate suites; there is no legacy feed
comparison, mirror table or alternate sync implementation. Optimized Cargo builds
are used. Ordinary checks do not run timings or require PostgreSQL.

| Suite | Command | Baseline | Scope |
| --- | --- | --- | --- |
| `contracts` | `make bench` | `benchmarks/baselines/local.json` | Schema normalization, value validation, projections and lossless JSON codec |
| `backend` | `make db-bench` | `benchmarks/baselines/backend-local.json` | Authorized bounded queries, command commits/retries and workspace contention |
| `cdc` | `make cdc-bench` | `benchmarks/baselines/cdc-snapshot-local.json` | pgoutput capture, bounded history, bootstrap and durable application |
| `sync` | `make sync-bench` | `benchmarks/baselines/sync-local.json` | Actual command table through the sole CDC path, at 1/512 tasks |

Each suite has a matching Make `*-compare` target. `python3 scripts/bench.py
compare --suite sync --strict` enables the review gate. Current reports go under
`target/benchmarks/`; running or comparing never overwrites baselines.

## Sampling and comparison

Individual request cases use five warmups and 31 measured samples, recording raw
samples, median, median absolute deviation and nearest-rank p95. This is a coarse
tail estimate, not a production SLO. Contention records 124 requests from four
writers synchronized per round, with no separate warmup. Its throughput includes
barrier/scheduling costs and does not measure maximum sustainable load.

Fast contract and borrowed-cache operations use batched averages to avoid timing
clock overhead as request tails. Contract workloads calibrate batch size after
warmup. Cache reads borrow 50 retained results after identity/argument checks;
they do not evaluate a new query or render a browser UI.

Comparisons reject incompatible suite, environment, compiler/build profile,
database profile, sample kind and fixture/workload metadata. New cases are
reported; removed baseline cases fail compatibility. A median slowdown is flagged
when it exceeds both 15% and 100 ns and is larger than three times the combined
MAD. Noise allowances are heuristics; inspect p95 and raw samples even when the
median gate passes. Review flags are advisory unless `--strict` is supplied.

Reports capture source/harness/runner fingerprints, Git HEAD, compiler, platform
and build configuration. Database suites record version, endpoint/database and
WAL/durability settings without credentials. Keep machine load, container
resources, cache state and connection placement consistent. Differences outside
recorded settings can still affect results. A baseline is specific to its machine.

## Authoritative backend

Queries return 50 rows at 1,000/100,000 tasks: status-filtered first page, indexed
UUID keyset continuation and explicit summary snapshots. Query plans include
`EXPLAIN (ANALYZE, BUFFERS)`. Check index choice, scanned rows, buffers and sorts,
not latency alone. Datasets have approximately balanced statuses; cold/skewed
workloads remain future coverage.

Commands cover create, one-field version-checked patch and identical receipt
replay. They include authorization, managed query revision and durable receipts.
There is no source delivery-log write. One-versus-four-workspace contention
exposes the remaining workspace coordination cost. Setup, connections, seeding
and cleanup are excluded. The fixed namespace owns UUID workspace fixtures.

## CDC stages

Capture measures a precommitted one-row or 100-row transaction, a SQL commit plus
capture through a persistent stream, and reading/decoding 100 retained batches.
The obsolete idle-poll case is removed; an idle stream waits for source progress.
Snapshot cases measure copying/initializing 1,000 rows and log-read/decode/application of a
100-row transaction to that replica. Copy excludes connection/slot creation/snapshot import;
application excludes SQL write/capture. Replica size checks use atomic counters.

Count/payload/page/WAL diagnostics at 64/128/256/512 one-row commits verify
bounded history behavior. Live encoded payload limits do not imply exact SQLite
file or WAL caps. Fixtures have generated schemas, publications and dedicated
slots; successful runs remove only their owned resources. See [capture](cdc-slice.md)
and [bootstrap/materialization limits](cdc-snapshot.md).

## Command-to-replica lifecycle

This suite publishes the actual `_hulladb_task_probe.tasks` table, including its
physical `(workspace,id)` identity. It seeds 1 or 512 workspace rows before slot
creation. No fixture mirror trigger, command-written log or secondary engine runs.

| Metric | Timed boundary |
| --- | --- |
| `bootstrap` | Connections, dedicated slot creation, snapshot export/import, source copy and durable replica initialization |
| `command_commit` | Authorized version-checked patch through PostgreSQL commit and returned receipt |
| `commit_to_replica` | After receipt return through streaming reception, durable event-log commit and atomic replica application; asynchronous upstream feedback is excluded |
| `command_to_replica` | Combined command and visibility stages, measured on the same operation |
| `replica_view` | Reading/decoding an internal bounded full-row view from durable SQLite |

Each bootstrap sample creates its own dedicated slot and imports that slot's
exported snapshot; the owned slot and replica file are removed between samples.
Slot cleanup is outside the timer. Unlike the earlier overlap fixture, the timed
bootstrap includes slot creation/export, so this fixture uses version three and
requires an explicit new baseline. Patch samples alternate completion, check versions and
verify the materialized row/cursor. Fixture creation, slot setup, validation and
cleanup are excluded. Bootstrap intentionally includes acquisition costs that the
isolated CDC copy case excludes.

These are local server measurements. Internal views are not authorized SDK query
results. Total latency excludes remote client RTT, pushed delivery, browser
startup/rendering and fan-out. The capture socket remains open across measured
operations. Upstream feedback follows durable event persistence asynchronously;
visibility does not wait for the feedback interval. Graceful shutdown waits for
reported feedback, outside the measurement. The CDC copy/application and sync fixtures use version three for exported
bootstrap. Capture-only fixtures remain version two; their timing boundaries are
unchanged. Versions and workload metadata reject comparisons across this handoff
change, as well as earlier SQL peek/advance measurements.
Do not interpret these timings as proof that Rust Wasm is faster than TS or that
server state is instantly visible to every client.

The local exported-bootstrap baseline recorded on 5 October 2026 reports these medians:

| Workspace tasks | Command acknowledgement | Acknowledgement to replica | Command through replica |
| --- | --- | --- | --- |
| 1 | 2.83 ms | 0.63 ms | 3.51 ms |
| 512 | 3.10 ms | 0.68 ms | 3.85 ms |

Bootstrap medians are 35.33 ms at one task and 30.51 ms at 512 tasks, including
slot creation/export/import. They cannot be compared directly with the earlier
bootstrap fixture, which excluded slot creation. Current CDC and sync baselines
record explicit replacement reasons and changed fixture/workload identities.

Each column is a separately computed median, so their sums need not match exactly.
The committed-versus-visible distinction remains even when local delivery is fast.

## Baseline updates

Save a separate baseline for another machine:

```sh
python3 scripts/bench.py save --suite sync --output benchmarks/baselines/my-sync.json
python3 scripts/bench.py compare --suite sync --baseline benchmarks/baselines/my-sync.json
```

Existing baselines require explicit replacement and a reason:

```sh
python3 scripts/bench.py save --suite cdc --replace \
  --reason "record the selected CDC implementation with atomic size accounting"
```

Review implementation and baseline changes together. Never automatically accept a
regression to silence the comparator. Change fixture metadata when workload or
timing boundaries change. Obsolete task-feed reports were removed during the
CDC-only consolidation; only current suites remain maintained.

Future coverage should measure authorized query/result derivation, dependency
invalidation, pushed delivery under realistic RTT/fan-out, receipt visibility
fences and browser Wasm startup/batched interop. Build time is secondary to
interactive latency and shipped runtime cost.
