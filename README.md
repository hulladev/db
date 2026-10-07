# Hulla DB

A Rust database platform with PostgreSQL as its authoritative backend and planned
Rust and TypeScript SDKs (`hulladb` and `@hulla/db`). Runtime responsiveness,
interactive queries/writes and client/server latency take priority over short
builds. See [architecture](docs/architecture.md) and [schema contracts](docs/schema-contract.md).

PostgreSQL remains authoritative. Managed commands authorize, validate and commit
business rows, synchronization revisions and recovery receipts in one transaction.
The sync service polls durable revisions and reevaluates authorized queries; native
clients store their authorized working sets in SQLite.

```text
Managed PostgreSQL transaction → durable revision → authorized query results → client SQLite
```

The serving path has no full server row mirror, replication connection or raw event
log. Client bootstrap chunks, rows, retained delivery history, delivery cursors and
visibility progress publish atomically. Stored dataset size is independent of
chunk working budgets.

Implemented today: schema/value/projection contracts, lossless JSON codec, a fixed
authoritative PostgreSQL command/query slice, managed revision-based task serving,
native client SQLite persistence and receipt-to-query visibility fences. Portable
delivery contracts have no backend drivers. The [task host](docs/task-host.md) adds
authenticated HTTP commands/receipts, SSE subscriptions, periodic polling and
connection recovery. SDKs, production role provisioning, general operation runtimes
and browser storage remain future work. Application writes must use managed operations; arbitrary external SQL is not
automatically synchronized.

The separate `hulladb_cdc` crate retains diagnostic logical capture/recovery and an
optional full-publication mirror behind `diagnostic-replica`. Its dependencies and
limits are not part of managed task delivery. See [managed synchronization](docs/managed-sync.md)
for correctness, recovery, source resets and the write boundary, and
[client persistence](docs/client-replica.md) for SQLite publication.

## Development

Install the pinned Rust 1.98.1 toolchain, Make and Python 3.9+. Docker Compose v2
is needed for database checks. SQLite is bundled through `rusqlite`.

```sh
rustup toolchain install 1.98.1 --profile minimal --component rustfmt --component clippy
make check
make db-up
make db-test sync-test server-test
make cdc-test              # Optional capture diagnostics.
```

Default builds exclude the diagnostic CDC crate; `--workspace` checks include it.

`make check` runs formatting, Clippy, workspace/doc tests and benchmark runner
checks without a database. Database cases are explicitly ignored in ordinary
Cargo tests. `make fmt` formats edits. Packages are unpublished; a license and
public API spelling remain open. Keep `Cargo.lock` for reproducible builds.

For incremental changes, run checks for the affected behavior and its consumers;
reserve the full suite for changes with broad regression risk. See
[agent guidance](AGENTS.md) for validation scope and test quality expectations.

## Performance

```sh
make bench                 # Schema/value/codec operations.
make db-bench              # Authoritative queries, commits, retries and contention.
make cdc-bench             # Capture/log and optional diagnostic mirror workloads.
make sync-bench            # Command-to-authorized-client lifecycle, 1/512 source tasks.
make sync-bench-compare     # Requires a new fixture-v6 managed client lifecycle baseline.
```

Suites record raw samples, median/MAD and coarse p95 for individual requests.
The sync suite separates command acknowledgement, commit-to-client persistence delay and
combined latency. It has no alternate engine, mirror table or extra source log.
Local native measurements do not establish network, subscription or browser
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
Compose to change the port. Logical WAL is enabled for optional diagnostics with a 256 MB slot-retained WAL
limit enforced at checkpoints. Lagging slots may expire; this is not an exact
instantaneous disk cap. Startup waits for readiness and preserves the data volume.
`db-down` removes the container/network while retaining that volume.

Initialization creates the current development schema and a stable source identity.
Until Cargo/npm publication, incompatible schemas and local fixtures are recreated
explicitly; no upgrade migrations or protocol-version compatibility are maintained.
Initialization never resets existing data. See the managed-sync maintenance/reset
contract. Database tests use owned UUID
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
| `crates/hulladb_protocol` | Portable scoped query and delivery contracts |
| `crates/hulladb_client` | Native client SQLite rows, bootstrap chunks, history and cursors |
| `crates/hulladb_sync` | Managed revision polling, authorized query maintenance and delivery |
| `crates/hulladb_server` | Authenticated HTTP/SSE, scheduled polling, pooling and recovery |
| `crates/hulladb_cdc` | Optional diagnostic PostgreSQL capture/log |
| `docs/architecture.md` | Requirements, selected boundaries and implementation gates |
| `docs/query-coverage.md` | Projection, bounded coverage, freshness and revocation contracts |
| `docs/benchmarks.md` | Current workloads, comparison rules and baseline policy |
| `scripts/bench.py` | Reproducible release benchmarks and comparison safeguards |
| `Makefile`, `compose.yaml` | Local checks and PostgreSQL development service |

The optional diagnostic bootstrap imports a slot-exported snapshot and rejects
unsupported schemas/publication shapes. Raw CDC logs and diagnostic mirrors are
privileged server data. Clients receive authorized logical projections only. The
fixed task slice has authenticated transport; general queries and SDK integration remain future work.
