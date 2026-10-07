# Performance workloads and baselines

Measure the retained execution model. Schema/codec operations, authoritative
commands/queries and CDC stages have separate suites. The raw server mirror is
measured only as an optional diagnostic; the sync suite uses client SQLite. Optimized Cargo builds
are used. Ordinary checks do not run timings or require PostgreSQL.

| Suite | Command | Baseline | Scope |
| --- | --- | --- | --- |
| `contracts` | `make bench` | `benchmarks/baselines/local.json` | Schema normalization, value validation, projections and lossless JSON codec |
| `backend` | `make db-bench` | `benchmarks/baselines/backend-local.json` | Authorized bounded queries, command commits/retries and workspace contention |
| `cdc` | `make cdc-bench` | `benchmarks/baselines/cdc-snapshot-local.json` | pgoutput capture, bounded history and optional diagnostic raw replica |
| `sync` | `make sync-bench` | `benchmarks/baselines/sync-local.json` | Commands, managed revision polling, authorized results and client SQLite, at 1/512 tasks |

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

## Command-to-client lifecycle

This suite uses the actual `_hulladb_task_probe.tasks` table, including its
physical `(workspace,id)` identity. It seeds 1 or 512 workspace rows before starting
managed query delivery, without a publication or replication slot. The server
reads authorized projections directly from PostgreSQL;
client SQLite stores the exact query results and delivery history. No full server
row mirror participates.

| Metric | Timed boundary |
| --- | --- |
| `bootstrap` | Feed/source verification, authorized initial query, subscription delivery, opening client SQLite and atomically storing the result/history/cursor |
| `command_commit` | Authorized version-checked patch through PostgreSQL commit and returned receipt |
| `commit_to_client` | After receipt return through durable revision polling, authorized PostgreSQL reevaluation, delivery and atomic client SQLite application |
| `command_to_client` | Combined command and client visibility stages, measured on the same operation |
| `client_query` | Reading the persisted authorized `id/title/completed` page and cursor from client SQLite |

Each bootstrap sample opens a fresh client file, removed after the sample.
Connections and fixtures are outside the timer. Patch samples call refresh immediately
after acknowledgement, so they do not include a periodic polling wait or an automatic
scheduler. Samples alternate completion, check versions and verify the client result;
setup, validation and cleanup are excluded. No capture connection or server event log
participates.

Fixture version six records managed transaction revision polling and client SQLite.
The committed `sync-local.json` baseline describes the earlier version-three server
mirror; version-five chunked CDC reports are historical too. Comparisons must reject
them. Establish a new machine-specific baseline explicitly before using this suite
as a regression gate. Do not compare old `*_replica` metrics to `*_client` metrics.
CDC capture/read and diagnostic copy/application fixtures use version four
after the chunked log/read change. Their retained older baselines are historical
and require explicit replacement before comparisons become regression gates.

These local measurements exercise the native client persistence core, without
network RTT, authenticated framing, browser startup/rendering or realistic
fan-out. They do not establish browser/Wasm performance or production latency.
Each metric has its own median; column medians need not add up exactly.

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
earlier CDC consolidation; fixture version six now measures the managed path.

Future coverage should measure selective dependency invalidation, pushed delivery
under realistic RTT/fan-out, receipt visibility
fences and browser Wasm startup/batched interop. Build time is secondary to
interactive latency and shipped runtime cost.
