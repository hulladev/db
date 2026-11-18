# Database architecture

Updated 6 November 2026. This document records the current direction; the
[SDK contract](sdk-architecture.md) gives the API and correctness requirements,
and the [delivery plan](sdk-implementation-plan.md) separates implemented work
from remaining work. Earlier PostgreSQL-only and online-only release proposals
have been replaced by this contract.

## Product and storage model

`@hulla/db` supplies reusable schema/function authoring, typed database operations,
reactive results and explicit replication/offline contracts. Rust and
TypeScript/JavaScript are first-class application languages on both clients and
servers. Application frameworks own routing, authentication flows, external
services and jobs.

PostgreSQL and SQLite are supported architectural choices. Engine selection and
replication are independent: SQLite may hold local-only data, readonly server
replicas, or authoritative server data. PostgreSQL is a server engine. There is
no implicit conversion from PostgreSQL to SQLite for development or disconnection.
libSQL/Turso-specific drivers require their own adapter and conformance evidence;
SQLite support alone does not establish compatibility with those services.

| Application layout | Authoritative storage | Device storage |
| --- | --- | --- |
| Local-only application | SQLite in the application | Same local database; no synchronization |
| Server-only application | PostgreSQL or SQLite | None required |
| PostgreSQL with offline devices | PostgreSQL | SQLite local tables and explicitly declared replicas |
| SQLite with offline devices | Server SQLite | Separate device SQLite databases and declared replicas |

This is the intended capability model. TypeScript SQLite storage and replica/offline
support are implemented; native Rust SQLite database execution and SQLite managed
HTTP hosting remain unfinished. Do not describe the SQLite managed server workflow
as having PostgreSQL parity yet.

## Declarations and application instances

Import `defineSchema`, `defineTable` and validators from `@hulla/db`. Declarations
are inert and reusable. Schemas own table names and expose `schema.query` and
`schema.mutation`; functions remain ordinary module exports selected by each
application. There is no additional `defineDatabase` or `defineFunctions` layer.

```ts
// db/schema.ts
import { defineSchema, defineTable, v } from '@hulla/db'

export const schema = defineSchema({
  drafts: defineTable({ title: v.string() }),
})
export const { query, mutation } = schema
```

```ts
// app/db.ts
import { sqlite } from '@hulla/db-sqlite'
import { schema } from '../db/schema'

export const db = sqlite({
  schema,
  filename: './app.sqlite',
  functions: () => import('./functions'),
})
```

`postgres({ schema, url, functions? })` from `@hulla/db-postgres` follows the same
instance model. The adapter constructor starts eagerly; operations await startup.
`ready()` is optional, state is observable, and `close()` explicitly terminates the
instance. Applications initialize once and share the instance across operations.
They do not connect/close around each query. Importing an application initializer
starts it; generation must inspect declarations without importing that initializer.

Ordinary startup opens and verifies storage. Explicit `.initialize(...)` provisions
it. Neither startup nor provisioning silently resets data or migrates incompatible
storage. Application credentials and provisioning credentials have different duties.
See [application instances and SQLite](sqlite.md) for executable examples and limits.

## Function ownership and execution

An instance optionally selects one barrel import loader or one build-time glob.
Barrels can combine shared and application functions through ordinary exports;
no arrays or automatic discovery of neighboring modules are necessary. A declaration
must belong to the instance's schema. Sharing declarations shares neither data nor
connections. Omitted registration exposes no local handlers.

Handlers use invocation-scoped `ctx.db`. Queries are readonly; mutations own one
transaction. Reusable helpers take the least database capability they need and
inherit the caller's transaction, snapshot and restrictions. They cannot escape
through a captured root connection. Direct application operations and explicit
transaction callbacks are also supported; external work belongs outside database
transactions wherever practical.

Managed remote TypeScript handlers execute in the restricted native worker. Local
application handlers run in the application's runtime and do not gain that worker's
sandbox or PostgreSQL retry policy. Current local handlers receive `identity: null`;
the managed host supplies verified identity. Do not infer remote exposure or a
security boundary from the database engine, filename, function declaration or build.

Generation emits browser-safe unbound references and source contracts under
`_generated/<entrypoint filename>`. Applications bind references with `db.bind(...)`.
Granular function imports and namespace imports are both supported. Handler code,
connection URLs and credentials never enter remote client artifacts. The optional
[`functionGlobs()` build integration](build.md) expands literal glob properties to
ordinary imports; it does not construct a second runtime or inject lifecycle code.

## Replicas, offline calls and conflicts

Standalone tables compose with `.extend()`, `.pick()` and `.omit()`. Bound source
tables retain their ownership; `.replica()` explicitly declares a readonly local
replica. Server query declarations specify their replica projection and authorize
which rows are returned. Projections retain primary keys. Local reads cover only
downloaded authorized rows; cached function results do not imply arbitrary row
coverage.

Instances supply typed `sources` entries containing generated source identity,
service URL and auth configuration. All replica owners must be supplied; missing,
extra, duplicate or mismatched sources reject before opening storage. Multiple
sources are valid. A storage URL and a service URL serve different purposes;
schema imports never carry endpoint configuration.

`.offline(resolveConflict)` opts a device-bound server mutation into durable queuing.
It queues the server call and can be combined with `.optimistic(...)` for temporary
feedback. It does not execute the server handler on the device or make replica
tables writable. Conflict resolution runs on the device; the application explicitly
chooses retry with revised input, rejection, or deferral to its own UI. There is no
default merge policy or built-in conflict UI. Server guards and authorization still
apply on every newly submitted attempt.

Sent attempts recover using their original request identity and receipt. A missing
receipt or lost response is not permission to resubmit. Unsent queued calls and
uncertain sent calls have different recovery paths. Durable queue/receipt evidence
survives cache eviction and requires explicit acknowledgement or removal.

## Result identity, completeness and freshness

Query results, local instance readiness and network connectivity are distinct.
Online query caches retain authorized coherent results and mark them stale during
transport interruption. Local replica queries read downloaded coverage, including
when offline. Neither result retention nor an optimistic layer proves a server
commit or full-table coverage. Per-source observable connectivity remains work;
current instance readiness must not be presented as network status.

Auth, source and tenant boundaries fence caches, subscriptions, replica views and
mutation recovery. A source/account change cannot leak old rows or replay a pending
write into a different database. Client auth keys partition state; only the server
verifier establishes identity. Unauthorized data is retired even when ordinary
infrastructure errors would retain a stale result.

## Storage and protocol invariants

Authoritative writes preserve constraints and ACID. Managed writes commit business
rows, revisions and successful receipts atomically. PostgreSQL currently implements
this server path. A SQLite server adapter must preserve the same externally visible
guarantees using SQLite transactions; it must not copy PostgreSQL isolation or
locking assumptions mechanically.

Managed synchronization uses transaction revisions and authorized query evaluation.
Arbitrary independent SQL writers are outside this contract. PostgreSQL CDC capture
and snapshots remain separate diagnostics, not a second production change source.
Local SQLite subscriptions observe writes through their instance and replica
materialization, not arbitrary external connections.

Restricted PostgreSQL mutations use bounded whole-handler OCC retries after
confirmed rollback. Direct application transactions run once unless retry is
explicitly enabled. Network uncertainty and business/concurrency conflicts are not
such automatic retries. See [concurrency](concurrency-and-optimism.md),
[wire format](wire-format.md), [execution limits](execution-limits.md) and
[query coverage](query-coverage.md) for detailed contracts and current capabilities.

Queues, caches, subscriptions and subprocess lifetimes are bounded. Eviction of
disposable results must never discard durable recovery evidence. Unsupported query
or driver capabilities reject explicitly. Codecs preserve exact values; UUID/primary
key rules, constraints and wire validation remain shared contracts across adapters.

## Packages and development tooling

| Responsibility | Boundary |
| --- | --- |
| Shared TypeScript authoring and contracts | `@hulla/db` |
| Application storage and engine integration | `@hulla/db-postgres`, `@hulla/db-sqlite` |
| Framework lifecycle bindings | Separate React, Vue, Svelte and Solid packages |
| Rust application API | `hulladb`, with optional `hulladb_postgres` and `hulladb_sqlite` adapters |

The Rust row describes the agreed public package direction, not a completed crate
refactor. Existing schema/macro/worker internals do not justify adding a mandatory
public runtime crate. Rust is not inherently a backend-only language.

CLI JavaScript, development runtimes, compilers, engine bridges and database server
executables have different footprints and lifetimes. Keeping a CLI entrypoint in
an SDK does not put it in an application bundle when runtime imports are isolated.
A PostgreSQL driver/bridge does not require installing the PostgreSQL server.
SQLite-only applications should not have to ship PostgreSQL or a development
compiler. Native asset relocation is not complete in the checkout.

**Under review:** retain DB-specific CLI/codegen with the SDK and have the existing
`hulla` CLI delegate to that project's installed version, without requiring another
manually coordinated tools package. The current executable remains `hulla-db`;
`hulla db` development/generation routing has not been implemented.

**Confirmed development behavior:** the dev command runs the supplied development
service against the selected storage. PostgreSQL without `url` or `DATABASE_URL`
uses managed persistent local PostgreSQL; an explicit connection uses that database.
An explicit initializer URL wins over the environment. Empty, invalid and unreachable
connections error without falling back to managed storage. External databases keep
explicit provisioning/migration; the development server runs in both cases. There
is no required local-PostgreSQL flag.

The PostgreSQL launcher accepts the exported adapter initializer. Generation prepares
server-only runtime settings without executing its eager constructor, and deployment
environment values are read when hosting starts. Outside dev, the adapter needs a
URL and never starts PostgreSQL. The older config workflow remains supported.

Automatic local provisioning and unconditional npm bundling are separate decisions.
Engine-specific download/cache distribution and first-run/offline setup remain
under review. Current development assets are still packaged. SQLite needs no
PostgreSQL process; managed SQLite HTTP hosting remains unfinished and rejects
explicitly before PostgreSQL provisioning. Production always requires configured
storage and never falls back to a local development database.

Convex demonstrates a managed local backend process under its dev command;
[its documentation](https://docs.convex.dev/cli/local-deployments) also allows cloud
development. Zero's dev command starts its cache, while
[its installation guide](https://zero.rocicorp.dev/docs/install) requires a separately
configured PostgreSQL database. These are different provisioning models.

## Delivery and operational evidence

The [delivery plan](sdk-implementation-plan.md) lists remaining implementation and
review gates. Existing PostgreSQL deployment proof does not establish SQLite
hosting, Rust SQLite execution, pooler compatibility or every platform's support.
Use [deployment](deployment.md) and [dependency boundaries](dependencies.md) for the
current shipped implementation, including assets still awaiting extraction.

Prefer one supported implementation per capability. Remove superseded APIs and
guidance when their replacement lands, preserving supported published paths during
an explicit migration. Git history stores retired proposals. Schema upgrades follow
[schema migrations](schema-migrations.md); release/protocol changes need an explicit
upgrade/recovery plan. Never reset storage silently.
