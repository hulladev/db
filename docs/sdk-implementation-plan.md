# Database delivery plan

Updated 6 November 2026. The governing contract is
[SDK architecture](sdk-architecture.md), with product/package boundaries in
[architecture](architecture.md). This replaces the October proof packets and
chronological implementation diary; completed work is not a new task list.

## Implemented foundation

| Area | Current behavior |
| --- | --- |
| Authoring | Inert core schemas/tables, schema-owned query/mutation helpers, immutable table composition and source-bound replicas |
| Application instances | Optional PostgreSQL/SQLite adapters, eager startup, automatic operation waits, observable readiness and optional terminal close |
| Function selection | One barrel loader or glob, schema ownership validation, instance-local bindings and aliases |
| Generation | Static entrypoint analysis, separate inert schema imports, generated source contracts and granular/namespace remote references; no eager initializer execution |
| PostgreSQL development | Initializer/config entrypoints, managed storage when URL is omitted, explicit external connections, runtime environment settings and explicit external provisioning |
| Application globs | Small opt-in `functionGlobs()` integration for Bun and Vite/Rollup; normal module imports and host build lifecycle |
| SQLite | Node/Bun built-in drivers and application-supplied browser/Expo connections; transactions, constraints, codecs, exact int64 and supported CRUD/query subset |
| Replica downloads | Server-authorized query selections, readonly local tables, overlapping coverage and auth/source fencing |
| Offline mutations | Explicit device-side conflict callback, optional optimism, durable per-source/account FIFO, receipt-only recovery for sent attempts |
| PostgreSQL hosting | Native managed transactions/revisions/receipts, restricted TypeScript handlers, authenticated HTTP/SSE and coherent result publication |
| Framework bindings | Shared callables/cache with separate React/Vue/Svelte/Solid lifecycle packages and framework recipes |

The SQLite result/recovery cache in the Rust remote client is implemented, but it
is not a Rust SQLite database driver. SQLite local handlers also do not inherit the
managed PostgreSQL worker sandbox, authorization setup or retry policy.

## Remaining work and review gates

### Engine-neutral Rust API and SQLite server execution

Review the Rust API before the crate refactor. The agreed public direction is
`hulladb` plus optional `hulladb_postgres` and `hulladb_sqlite`; do not introduce a
mandatory `hulladb_runtime` crate. Rust applications may be clients, servers or both.

Remove PostgreSQL-specific execution types from shared authoring/registry/hosting
contracts, then implement the SQLite database adapter and managed HTTP hosting.
Preserve authorization, revisions, receipt atomicity, transaction scopes and recovery
semantics across engines. SQLite serializes writes differently from PostgreSQL;
prove the required behavior rather than promising identical isolation internals.

Required evidence includes persisted CRUD, constraints and rollback, authorized
replica projections, uncertain commit recovery, duplicate admission protection,
auth/source changes, restart and coherent subscriptions for each supported engine.
Do not infer Rust or managed SQLite parity from TypeScript local-driver tests.

### Package assets and command ownership

Complete extraction of PostgreSQL-specific production assets from the core package.
Keep production driver/bridge assets separate from development compilers, private
runtimes and local database server executables. SQLite consumers should not receive
PostgreSQL server assets merely by installing the shared SDK.

The latest recommendation is SDK-owned DB tooling delegated to by the existing
`hulla` CLI, using the project's installed SDK version. Exact routing, native asset
distribution and platform packaging remain under review. There is no approved
extra `db-tools`/`db-cli` installation, new CLI flag or download/cache implementation.
Node/Bun portability and standalone CLI distribution remain valid implementation
options; the current Bun-only tooling is an implementation constraint to address,
not a permanent architecture requirement.

`hulla-db dev` now accepts PostgreSQL adapter initializer entrypoints as well as
existing configs. Omitted URLs select managed local storage; explicit connections
select external storage without fallback or automatic provisioning. Server settings
retain runtime environment expressions without opening the application's instance.
SQLite managed HTTP hosting is still unsupported and rejects before PostgreSQL
provisioning. Finish that engine's hosting path without substituting another engine.
Production requires configured storage and never silently provisions a development
database. Development binary delivery still needs extraction from current packaging.

### Remaining instance and SQLite capabilities

- Expose per-source observable connectivity independently of local readiness and
  query freshness. Final connection-state API still needs review.
- Implement SQLite schema migrations explicitly; ordinary opening never applies
  migrations or recreates incompatible storage.
- Extend SQLite queries only with conformance evidence. JSON predicates, text
  matching modes, grouping, pagination and custom key handlers currently have
  explicit limitations listed in [the SQLite guide](sqlite.md).
- Finish managed SQLite service/development parity before documenting equivalent
  server startup across adapters.

### Documentation and cutover

The [SQLite/schema guide](sqlite.md) describes the current instance API.
The older `defineConfig`, `hulla-db` and generated `/server` setup remains available
for the existing PostgreSQL service workflow while its migration is incomplete;
those guides must be labeled accordingly. Do not introduce new applications using
that setup as the engine-neutral target or remove supported exports solely because
new authoring has landed.

Migrate remaining operational configuration and generated consumers with their
actual replacement. No compatibility facade is needed solely to archive an
unreleased proposal. Remove superseded code, fixtures and instructions together;
retain distinct behavior tests and public contracts still in use.

## Validation and publication

Choose checks from the actual diff. Preserve independent evidence for type safety,
real storage transactions, browser bundle boundaries, Node/Bun execution, auth races,
receipt recovery and engine-specific semantics. Documentation-only edits require
diff/link review, not another full database test run.

Package validation must include both adapters, fixed-version family alignment,
dependency-ordered installation/publication and installed consumer smoke checks.
If platform-specific assets are split, adapt archive verification so it does not
try to install incompatible platforms as direct application dependencies.

Before release, exercise prepared hosting without a compiler, local development
from installed packages, explicit external database selection and orderly shutdown.
Measure representative production bundles and installed/archive contents separately;
a CLI file present on disk is not necessarily loaded by the application.

PostgreSQL has local and recorded hosted deployment evidence. SQLite managed
hosting, native Rust SQLite execution and new asset-delivery arrangements need their
own evidence. Existing success does not establish sustained-load or transaction
pooler compatibility. Do not claim unrun integration or release checks passed.
No commit, push, release or external deployment is implied by this plan.
