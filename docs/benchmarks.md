# Performance workloads and baselines

Measure the retained execution model. Schema/codec operations and diagnostic CDC stages have separate suites. The raw
server mirror is measured only as an optional diagnostic. Optimized Cargo builds
are used. Ordinary checks do not run timings or require PostgreSQL.

| Suite | Command | Baseline | Scope |
| --- | --- | --- | --- |
| `contracts` | `make bench` | `benchmarks/baselines/local.json` | Schema normalization, value validation, projections and lossless JSON codec |
| `cdc` | `make cdc-bench` | `benchmarks/baselines/cdc-snapshot-local.json` | pgoutput capture, bounded history and optional diagnostic raw replica |

Each suite has a matching Make `*-compare` target. `python3 scripts/bench.py
compare --suite cdc --strict` enables the review gate. Current reports go under
`target/benchmarks/`; running or comparing never overwrites baselines.

## Sampling and comparison

CDC request cases use five warmups and 31 measured samples, recording raw samples,
median, median absolute deviation and nearest-rank p95. This is a coarse tail
estimate, not a production SLO. Fast contract operations use calibrated batched
averages to avoid measuring clock overhead as request tails.

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

The retained CDC baseline predates chunked storage. Current capture/read and
diagnostic copy/application fixtures require an explicitly refreshed baseline
before comparisons become regression gates.

## Baseline updates

Save a separate baseline for another machine:

```sh
python3 scripts/bench.py save --suite cdc --output benchmarks/baselines/my-cdc.json
python3 scripts/bench.py compare --suite cdc --baseline benchmarks/baselines/my-cdc.json
```

Existing baselines require explicit replacement and a reason:

```sh
python3 scripts/bench.py save --suite cdc --replace \
  --reason "record the selected CDC implementation with atomic size accounting"
```

Review implementation and baseline changes together. Never automatically accept a
regression to silence the comparator. Change fixture metadata when workload or
timing boundaries change. Retired application fixtures and their baselines must be removed together.

Future coverage should measure selective dependency invalidation, pushed delivery
under realistic RTT/fan-out, receipt visibility
fences and browser Wasm startup/batched interop. Build time is secondary to
interactive latency and shipped runtime cost.
