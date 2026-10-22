# Hulla DB

A Rust database platform with PostgreSQL as its authoritative backend and initial
Rust and TypeScript SDKs (`hulladb` and `@hulla/db`). Runtime responsiveness,
interactive queries/writes and client/server latency take priority over short
builds. See [architecture](docs/architecture.md) and [schema authoring](docs/schema-authoring.md).

PostgreSQL remains authoritative. Managed commands authorize, validate and commit
business rows, synchronization revisions and recovery receipts in one transaction.
The sync service polls durable revisions and reevaluates authorized queries; native
clients store their authorized working sets in SQLite.

```text
Managed PostgreSQL transaction → durable revision → authorized query results → client SQLite
```

The serving path uses managed PostgreSQL functions and authorized result snapshots.
Source identities and scoped revisions correlate committed mutations with visible
query results. Native clients cache function results and recover pending mutations
in SQLite; browser clients maintain live results in memory.

Implemented today: schema/value/projection contracts, lossless JSON codecs, managed
PostgreSQL functions, authenticated HTTP/SSE and Rust/TypeScript SDKs.
The [TypeScript SDK](packages/db/README.md) adds schema
authoring, generated table types and Rust schema contracts, callback declarations,
browser-safe direct-import generation and runtime/dispatch boundaries.
The [direct server database](docs/server-database.md) adds generated database access,
lazy CRUD builders and scoped transactions sharing managed storage and revisions.
The [supplied browser service](docs/browser-service.md) discovers declarations,
generates worker wiring and serves them with application-owned authentication.
The [managed PostgreSQL function runtime](docs/managed-functions.md) adds
schema-driven tables/constraints, CRUD, registered Rust queries and mutations,
tracked snapshot reads, atomic revisions and recovery receipts.
The [deployed function adapters](docs/sdk-adapters.md) add generic HTTP/SSE serving,
generated result codecs, in-memory query caching and mutation confirmation, subscription
reconnects and identity cleanup. Identical watches share query evaluations within
each host; idle polls recheck policy while reusing handler results at unchanged
revisions. The [Rust authoring API](docs/rust-authoring.md)
adds table/input derives, typed contexts and selectors, query/mutation attributes,
registration adapters and direct calls with owned watches. The optional
[TypeScript transaction worker](docs/typescript-functions.md) executes authored
callbacks through schema-typed CRUD with native policies and atomic receipts.
The [Rust remote client](docs/rust-remote-client.md) adds client-only bindings,
verified HTTP/SSE, SQLite result caching and durable same-UUID recovery.
The [transactional outbox](docs/outbox.md) commits external jobs with mutations,
with leased workers, bounded retries, dead letters and stable idempotency keys.
[Operational visibility](docs/operations-visibility.md) adds host counters, scope
diagnostics and bounded receipt compaction that preserves request identities.
Local browser table replicas remain future work. The
[production operations starter](docs/production-readiness.md) supplies role provisioning,
durable quotas, framework request scopes, typed admin transactions, backup/restore
and self-hosted containers. The [intended developer API](docs/backend-functions.md) describes
the broader authoring model.
Application writes must use managed operations; arbitrary external SQL is not
automatically synchronized.

The separate `hulladb_cdc` crate retains diagnostic logical capture/recovery and an
optional full-publication mirror behind `diagnostic-replica`. Its dependencies and
limits are separate from managed function serving. See [managed functions](docs/managed-functions.md)
for transactions and recovery, [deployed adapters](docs/sdk-adapters.md) for serving,
and [Rust remote clients](docs/rust-remote-client.md) for durable result caching.

## Install

Install the core SDK and the binding for your application:

```sh
npm install @hulla/db@0.1.0 @hulla/db-react@0.1.0 zod
npm install --save-dev typescript@7.0.2
```

Use `@hulla/db-vue`, `@hulla/db-svelte` or `@hulla/db-solid` for other UI
frameworks. Add `@hulla/db-better-auth` when Better Auth stores its records in this
database. The [installation guide](https://hulla.dev/docs/db/installation) covers
runtime requirements and framework choices.

For native Rust applications:

```sh
cargo add hulladb@0.1.0
```

The npm SDK includes macOS arm64 and Linux x64/glibc native executables. Server
access requires Node 22.18+ or Bun; generation and the development watcher require
Bun and TypeScript. Browser bindings do not launch native executables.

## Development

Install the pinned Rust 1.98.1 toolchain, Make and Python 3.9+. Docker Compose v2
is needed for database checks. SQLite is bundled through `rusqlite`.

```sh
rustup toolchain install 1.98.1 --profile minimal --component rustfmt --component clippy
make check
make db-up
make client-test            # Native HTTP/SSE and TLS regressions.
make db-test server-test     # Managed PostgreSQL functions and HTTP/SSE integration.
make remote-functions-check  # Client-only Rust bindings and durable cache/recovery.
make production-test        # Role/quota/admin database checks and backup drill.
make rust-authoring-test   # Rust derives, callables and typed transactions.
make sdk-adapters-test     # Generated TypeScript calls against HTTP/SSE/PostgreSQL.
make typescript-functions-test # Authored TypeScript handlers through managed transactions.
make cdc-test              # Optional capture diagnostics.
```

Default builds exclude the diagnostic CDC crate; `--workspace` checks include it.

`make check` runs formatting, Clippy, workspace/doc tests and benchmark runner
checks without a database. Database cases are explicitly ignored in ordinary
Cargo tests. `make fmt` formats edits. Packages are licensed under MIT.
Keep `Cargo.lock` for reproducible builds. See [release preparation](docs/releasing.md)
for the npm and Cargo archive workflow.

For incremental changes, run checks for the affected behavior and its consumers;
reserve the full suite for changes with broad regression risk. See
[agent guidance](AGENTS.md) for validation scope and test quality expectations.

## Performance

```sh
make bench                 # Schema/value/codec operations.
make cdc-bench             # Capture/log and optional diagnostic mirror workloads.
```

Suites record raw samples, median/MAD and coarse p95 for individual requests.
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
Initialization never resets existing data. See the [managed runtime](docs/managed-functions.md)
and [restore procedure](docs/production-readiness.md). Database tests use owned
schemas/publications/slots and temporary SQLite files. Successful runs clean
up their fixtures; failed runs can leave diagnostic resources. Never reset an
existing slot or database to hide a failure. See [backend guarantees](docs/managed-functions.md),
[capture recovery](docs/cdc-slice.md) and [bootstrap limits](docs/cdc-snapshot.md).

The volume follows the [PostgreSQL 18+ image layout](https://hub.docker.com/_/postgres).
Changing PostgreSQL major versions requires a database upgrade.

## Codebase

| Path | Responsibility |
| --- | --- |
| `packages/db` | Schema authoring/types, Rust contract emission, generated callables and browser HTTP/SSE recovery |
| `packages/react`, `vue`, `svelte`, `solid` | Separate UI bindings, live pagination and SSR initial data |
| `packages/better-auth` | Optional Better Auth PostgreSQL storage and schema support |
| `crates/hulladb` | Rust derives, typed contexts, direct calls, embedded and remote runtimes |
| `crates/hulladb_macros` | Table/input derives and query/mutation registration/call generation |
| `crates/hulladb_schema` | Logical table/key/reference/unique constraints, validation and projections |
| `crates/hulladb_wire` | Lossless tagged JSON values and record codec |
| `crates/hulladb_postgres` | Schema-driven managed functions/CRUD, atomic revisions and receipts |
| `crates/hulladb_protocol` | Portable scoped query and delivery contracts |
| `crates/hulladb_client` | Reusable native HTTP/SSE transport with verified TLS |
| `crates/hulladb_server` | Authenticated HTTP/SSE, scheduled polling, pooling and recovery |
| `crates/hulladb_cdc` | Optional diagnostic PostgreSQL capture/log |
| `docs/architecture.md` | Requirements, selected boundaries and implementation gates |
| `docs/query-coverage.md` | Entity projections and exact function-result coverage |
| `docs/benchmarks.md` | Current workloads, comparison rules and baseline policy |
| `scripts/bench.py` | Reproducible release benchmarks and comparison safeguards |
| `Makefile`, `compose.yaml` | Local checks and PostgreSQL development service |

The optional diagnostic bootstrap imports a slot-exported snapshot and rejects
unsupported schemas/publication shapes. Raw CDC logs and diagnostic mirrors are
privileged server data. Clients receive authorized logical projections only. The
generic function host and TypeScript adapter serve registered native functions.
Rust-authored handlers use generated row/input/ID types and typed contexts.
TypeScript callbacks execute through the optional restricted JavaScript worker bridge;
client-only Rust bindings and durable native function clients use the optional
`hulladb/remote` feature.
