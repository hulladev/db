# Hulla DB

A Rust database platform with PostgreSQL as its authoritative backend and planned
Rust and TypeScript SDKs (`hulladb` and `@hulla/db`). Runtime responsiveness,
interactive queries/writes and client/server latency take priority over short
builds. See [architecture](docs/architecture.md) and [schema contracts](docs/schema-contract.md).

CDC is the sole PostgreSQL synchronization model:

```text
PostgreSQL commit → Rust CDC → durable bounded event log → durable replica
                                                    → authorized queries → SDKs
```

Commands authorize, validate and commit rows plus recovery receipts, then return.
They do not wait for replica visibility or write a separate delivery feed. The
previous task feed, RAM replica, mirror-based comparison and obsolete baselines
have been removed. The selected client direction is a shared Rust query/replica
core, native for Rust and through Wasm for TypeScript, with thin TS integration.
SDKs and Wasm bindings are not implemented or benchmarked yet.

Implemented today: logical schema/value/projection contracts, a lossless JSON
codec, fixed PostgreSQL commands and bounded queries, pgoutput transaction capture,
bounded MVCC bootstrap, SQLite event history and atomic durable materialization.
Capture uses a persistent PostgreSQL replication connection. Exported snapshots,
authorized CDC query serving, receipt-to-visibility correlation and pushed client
delivery remain the next layers of this direction. The current server materialization
does not establish that an additional durable server copy is mandatory in production.

## Development

Install the pinned Rust 1.98.1 toolchain, Make and Python 3.9+. Docker Compose v2
is needed for database checks. SQLite is bundled through `rusqlite`.

```sh
rustup toolchain install 1.98.1 --profile minimal --component rustfmt --component clippy
make check
make db-up
make db-test cdc-test
```

`make check` runs formatting, Clippy, workspace/doc tests and benchmark runner
checks without a database. Database cases are explicitly ignored in ordinary
Cargo tests. `make fmt` formats edits. Packages are unpublished; a license and
public API spelling remain open. Keep `Cargo.lock` for reproducible builds.

## Performance

```sh
make bench                 # Schema/value/codec operations.
make db-bench              # Authoritative queries, commits, retries and contention.
make cdc-bench             # Capture, event log, bootstrap and durable application.
make sync-bench            # Actual command-to-CDC replica lifecycle, 1/512 tasks.
make sync-bench-compare     # Compare the retained execution model to its baseline.
```

Suites record raw samples, median/MAD and coarse p95 for individual requests.
The sync suite separates command acknowledgement, commit-to-replica delay and
combined latency. It has no alternate engine, mirror table or extra source log.
Local streaming measurements do not establish network, subscription or browser
latency. See [methodology and baseline updates](docs/benchmarks.md).

## Local PostgreSQL

```sh
make db-config
make db-up
make db-shell
make db-down
```

Compose uses `postgres:18.6-bookworm` on `127.0.0.1:55432`, with database/user/password
`db_dev`, for development only:

```text
postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev
```

Use `HULLADB_TEST_URL` for another instance. Set `DB_PORT=55433` when starting
Compose to change the port. Logical WAL is enabled with a 256 MB slot-retained WAL
limit enforced at checkpoints. Lagging slots may expire; this is not an exact
instantaneous disk cap. Startup waits for readiness and preserves the data volume.
`db-down` removes the container/network while retaining that volume.

Initialization upgrades the earlier internal task schema, preserving tasks and
receipts and removing the old delivery table. Database tests use owned UUID
workspaces/publications/slots and temporary SQLite files. Successful runs clean
up their fixtures; failed runs can leave diagnostic resources. Never reset an
existing slot or database to hide a failure. See [backend guarantees](docs/postgres-slice.md),
[capture recovery](docs/cdc-slice.md) and [bootstrap limits](docs/cdc-snapshot.md).

The volume follows the [PostgreSQL 18+ image layout](https://hub.docker.com/_/postgres).
Changing PostgreSQL major versions requires a database upgrade.

## Codebase

| Path | Responsibility |
| --- | --- |
| `crates/hulladb_schema` | Logical table/key/reference/unique constraints, validation and projections |
| `crates/hulladb_wire` | Lossless tagged JSON values and record codec |
| `crates/hulladb_postgres` | Authoritative commands, receipts, indexed queries and exact query caches |
| `crates/hulladb_cdc` | Sole PostgreSQL capture/bootstrap/log/replica execution path |
| `docs/architecture.md` | Requirements, selected boundaries and implementation gates |
| `docs/query-coverage.md` | Projection, bounded coverage, freshness and revocation contracts |
| `docs/benchmarks.md` | Current workloads, comparison rules and baseline policy |
| `scripts/bench.py` | Reproducible release benchmarks and comparison safeguards |
| `Makefile`, `compose.yaml` | Local checks and PostgreSQL development service |

The current bootstrap is bounded and rejects unsupported XID epochs, schemas and
publication shapes. Raw server logs/replicas have no client authorization boundary.
Completing the serving layer is required before these internals become a SDK API.
