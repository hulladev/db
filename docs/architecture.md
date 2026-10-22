# Hulla DB — architecture and implementation design

**Status:** Architecture proposal with schema/value/projection contracts, lossless JSON codecs, schema-driven PostgreSQL functions, atomic receipts and managed revisions implemented. Authenticated HTTP/SSE serving supports Rust and TypeScript SDKs. Native function clients use verified TLS and durable SQLite result/recovery storage; browser clients keep live results in memory. TypeScript declarations execute through the restricted transaction worker, while server code can use direct schema-typed CRUD. CDC capture and raw snapshots remain separate diagnostics. The fixed task prototype has been retired. Local browser row replicas remain deferred; the implementation sections below distinguish current behavior from future gates.

**Updated:** 12 October 2026

**Audience:** Product owner, future implementation agents, and contributors

## 1. Product direction

Package naming: `@hulla/db` for npm and `hulladb` for the Rust SDK. Supporting
Rust crates use the `hulladb_` prefix, such as `hulladb_schema`.

Build a self-hostable reactive database platform with a Rust core, PostgreSQL as its first authoritative storage backend, and first-class Rust and TypeScript SDKs.

Applications use reactive server query results and can optionally apply optimistic writes for immediate mutation feedback. The server executes queries, determines which writes commit and controls which data each client may receive. Disconnected mutation authoring and later conflict merging are outside the initial product contract.

The first-release developer experience uses integrated schemas, typed operations and reactive subscriptions to server function results. Local row querying is a deferred possible capability. Storage adapters must allow SQLite and related engines to be added without redesigning the protocol, subscription engine, or SDK APIs.

This is initially a database platform built around an existing storage engine. Building a new on-disk storage engine is not required to deliver the product.

## 2. Requirements and decision status

### Confirmed requirements

- Rust implementation for the platform core.
- First-class Rust and TypeScript/JavaScript SDKs.
- PostgreSQL as the initial default server database.
- Architectural boundaries that accommodate SQLite, libSQL, and Turso Database adapters later.
- End-to-end type safety between declared tables, operations, and SDK usage.
- `id` is optional in a table declaration; omitting it adds a UUID field with
  UUID generation using the schema-wide v4/v7 default (v7 when unconfigured).
  The primary key defaults to `id`, whether implicit
  or explicitly declared with an overridden type. Developers can explicitly
  select another primary key and override generation logic independently.
- Selecting another primary key suppresses implicit `id` injection; explicitly
  declared fields are preserved.
- The initial logical contract supports single-field UUID, string, and signed
  64-bit integer primary keys. Boolean keys are rejected; composite primary keys
  require later tuple/reference support. Multi-field business uniqueness remains
  a separate constraint requirement and is not satisfied by a generated row ID.
- Tables can declare named single-field or multi-field unique constraints.
  Nullable unique fields require an explicit null comparison policy. Structural
  normalization is implemented; storage must enforce these constraints atomically
  under concurrent inserts/updates and validate existing data during activation.
- Each table can allow caller-supplied insert keys or require automatic generation.
- UUID primary keys inherit a configurable schema-wide UUIDv4 or UUIDv7 default,
  with UUIDv7 when unconfigured. Explicit table-level generator settings take
  precedence, including disabled generation and custom handlers. String and integer keys require
  caller-supplied values unless a compatible generator is configured. Inserts
  accept explicit keys by default and generate omitted keys when possible.
- The selected primary key is immutable in ordinary updates. Updates cannot
  supply that field, even with its current value. Any future key-change operation
  must explicitly define relationship and synchronization behavior.
- Schema fields use declarative validator descriptors, and relationships are
  explicitly defined by the application developer.
- References require referential integrity. Non-null references must target
  existing authoritative rows, and referenced-row deletion is restricted in the
  initial contract. Automatic cascade/set-null behavior requires an explicit
  later contract; the initial slice has no dangling-reference opt-out.
- Authoritative writes must preserve ACID. Reference enforcement must be atomic
  with the write and remain correct under concurrent commands. Schema or client
  validation does not replace storage constraints and transaction isolation.
- A live development workflow that watches schema changes and regenerates runtime schemas, both SDKs' types and validates affected handlers. Release migrations are deferred until publication.
- Server-authoritative transactions, permissions, and business rules.
- Application writes use managed transactions, with synchronization tracking atomic
  with data and permissions. Database integrity takes priority over latency gains.
  Arbitrary independent SQL writers are outside the initial live-sync contract.
- Application-defined concurrency behavior, with safe built-in defaults and
  operation-level policies for specialized workloads. Collaboration must use
  the same authoritative mutation and sync foundations as single-client use.
- Reactive server queries with typed function-result subscriptions in both SDKs.
- Both optimistic local-plus-server writes and server-confirmed writes, selectable by the application developer.
- Optimistic changes reconcile to server results and roll back on definitive rejection.
- No requirement to accept new writes while disconnected.
- Free self-hosting, with a possible paid managed hosting offering.
- Client data exposure must be controlled on the server.
- A lean production package and low runtime overhead. End-user query/write
  responsiveness and client/server latency take priority over build/compile speed.
- TypeScript declaration shape and default concurrency semantics are confirmed.
  Restricted declared mutations use bounded automatic OCC retries; ordinary
  server transaction callbacks run once by default. Retry limits and backoff
  defaults remain open.

### First-release read model

Decision, confirmed on 12 October 2026: ship reactive server queries first.
Applications declare schemas, write typed query/mutation functions and consume
server-evaluated query results through generated calls and watches. This supersedes
the earlier confirmed requirement for local row querying in the initial release.
Authorized local row querying, its coverage model and a browser replica are outside
the selected release. Superseded implementations are removed during consolidation,
not maintained as a parallel product path.

The choice keeps query execution and authorization together and avoids making local
row coverage part of ordinary application use. Optional optimism can address mutation
latency without executing server callbacks locally. Cached function results do not
establish coverage for new filters or arguments. The confirmed cache policy below
waits for the server on a cold read and retains results while their live subscription
maintains freshness. Storage/retention mechanics and the typed optimism API remain
open; this decision changes neither the write presentation default nor
transaction/recovery guarantees.

### Implementation consolidation and hosting

Decision, confirmed on 12 October 2026: replace superseded implementations rather
than maintain old and new paths. Remove obsolete source, exports, dependencies,
fixtures, benchmarks, build targets and documentation as the replacement lands.
Move meaningful regression coverage to the supported path; do not preserve a legacy
runtime solely to keep its tests. Preserve shared components used by the selected
design, including transaction integrity and mutation recovery. Rust and TypeScript
SDKs remain required; they are not competing implementation generations.

Git history is the archive for retired designs. Do not add legacy directories,
compatibility facades or feature flags merely to keep the previous implementation.
Verify that any unique uncommitted work is recoverable before deleting it. Historical
implementation notes below describe the checkout pending consolidation, not a mandate
to preserve obsolete paths. Future local-row support would require a new decision;
do not maintain its alternative design or speculative extension code in the meantime.

The standard deployment is a supplied Hulla DB service that discovers and runs
application schemas/functions. TypeScript applications author TypeScript; the package
owns native hosting, function registration, worker management and subscriptions.
Application frameworks and serverless functions call this service. Executor lifecycle
and the final authentication/authorization authoring API still require design.

### Database execution and application server code

Decision revised 12 October 2026: support ordinary server-only database calls and
`db.transaction(callback)` alongside declared `query` and `mutation` functions for
browser access. Server applications use the typed database directly and own their
API routes or framework functions, optionally through `@hulla/api`. They need not
declare a client-facing function for every database operation. Ordinary server
transaction callbacks run once by default, with no automatic callback replay.
External API calls and orchestration belong in application backend code; Hulla DB
is not a general application job runner.

Declared queries and mutations execute within an enforced restricted executor.
Normal calculations, transformations and compatible pure dependencies are supported.
Database access goes through scoped `ctx.db`; arbitrary network access and filesystem
writes are unavailable. Ambient time, randomness and shared mutable state must not
compromise tracked reads or safe transaction replay; supported generated-value APIs
have runtime-controlled semantics. Enforcement covers imported dependencies, rather
than relying only on types, linting or an author's claim that a handler is replay-safe.
The current trusted TypeScript worker does not yet enforce this boundary.

Queries use a consistent read-only snapshot and are eligible for reactive result
subscriptions. Declared mutations use serializable transactions and bounded automatic
whole-handler OCC replay after confirmed rollback. Ordinary server calls and declared
functions share the database engine, storage constraints, atomic commit, revisions
and reactive invalidation. Choosing ordinary server code changes callback execution
and retry behavior, not database integrity. Keep external work outside the open
database transaction wherever practical; rollback cannot undo an external effect.

Decision confirmed 12 October 2026: declarations are browser-facing entry points;
reuse ordinary database helpers across declarations and application server code.
There is no server `db.run`, `tx.run`, declaration `ctx.run`, or server `db.withAuth`
in the target API. Start with inline handlers and extract helpers only when reuse
is needed. This supersedes the earlier declaration-invocation and nested-savepoint
proposal; it does not describe APIs already implemented in the current source.

Helpers accept the minimum database capabilities they need. `ReadDatabase` exposes
read operations; `WriteDatabase` includes reads and writes; `Database` is the full
server API, including transaction management. Writable handles satisfy read-only
helper parameters. Queries supply read-only `ctx.db`, mutations supply writable
`ctx.db`, and server transactions supply writable `tx`. The full API remains bounded
by connection permissions, database constraints and execution-scope rules. Runtime
enforcement prevents a type cast from granting writes inside a query.

A helper uses its caller's snapshot or transaction, preserving tracked reads and
read-your-writes behavior. It does not introduce an independent commit, automatic
savepoint or retry boundary. An uncaught error rolls back the enclosing transaction;
catching an ordinary helper error does not itself undo earlier successful writes.
OCC failures invalidate the whole transaction even when caught. Restricted declared
mutations can replay the entire handler after confirmed rollback; ordinary server
transaction callbacks remain run-once by default. Helpers inherit the caller's
execution restrictions and cannot escape through a captured root connection or
invoke external services from a restricted declaration. Applications establish
identity and permissions at their own server endpoints, optionally sharing ordinary
permission helpers with declarations.

Root database execution inside its transaction callback must fail before executing
the operation, including CRUD, another root transaction and captured-root
helper calls. This guard follows the async execution scope; unrelated concurrent
requests remain independent. Transaction handles reject use after completion.
Validate enforcement and context propagation on supported runtimes before claiming
these guards work universally.

### Browser exposure and framework integrations

Only `query` and `mutation` are function declarations. There is no standalone
transaction declaration, action declaration, or per-function `internal`/`expose`
setting. Discovery registers exported declarations in the configured functions
directory; there is no special `api.ts` registry. Registration alone does not mount
network endpoints. An application separately mounts the generated query/mutation
transport when it wants direct browser access. Ordinary helpers and the server-only
database entry point do not become browser endpoints through discovery.

Generated browser bindings call mounted queries and mutations directly, with query
watches on the binding. Handlers execute on the server and enforce business access
rules and permitted outputs. Server-only imports and framework directives never
establish identity or grant administrative privilege. The service validates calls
and enforces the mounted surface independently of generated client files.

Framework support includes React, Vue, Svelte, Solid and Astro, and integrations
such as Next.js, Nuxt, SvelteKit and SolidStart. Shared SDK code owns transport,
live-result state and mutation recovery; UI bindings adapt reactivity and lifecycle.
Applications connect their existing auth lifecycle to the framework-independent
browser auth-state API; framework/provider combinations are not separate required
integrations. UI bindings and optional hosting helpers connect existing routes and
component lifecycles. No automatic framework request discovery is needed for server
database operations. These integrations do not replace the application's router.
A browser-only frontend still uses the @hulla/db service to execute its declared
queries and mutations; external work requires an application backend/API endpoint or
framework function.

### Recommendations requiring eventual product approval

The choices below guide this proposal; confirmed decisions are identified explicitly:

1. Native schema authoring in Rust or TypeScript, compiled into one language-neutral internal contract that drives both SDKs and storage definitions. Each project chooses one authoritative authoring source.
2. First release: subscribe to named server query results. Broader dataset preloading and local query coverage belong to the deferred local-read design.
3. Separate confirmed client data and optimistic layers.
4. Managed mutations and durable transaction revisions are confirmed on 6 October
   2026. External writers require controlled maintenance/reset; a future connector
   is a separate product decision, not an alternate live source for the same data.
5. Authorized query maintenance reads PostgreSQL snapshots and polls their durable
   revisions. No mandatory WAL capture or full server row mirror participates.
6. A constrained portable server database query model, with explicit backend-specific extensions. Local execution conformance is deferred with local row querying.
7. Preserve backend-independent client contracts and explicit resource budgets.
   Existing native clients use SQLite; first-release persistence defaults remain open.
   Durable local row querying and browser replica storage are deferred.
8. Server-confirmed writes as the safe initial default, overridable per operation and call.
9. One authoritative write region per deployment initially.

An implementation agent must preserve confirmed requirements, identify changes to these recommendations, and avoid treating illustrative API spelling as a specification.

### Runtime priorities and production consolidation

Developer-experience direction, clarified on 7 October 2026: application authors
declare table schemas, define queries/functions and use a generated client API
plus a typed server/admin database. SDKs and generated code own transport framing,
request identities, receipts, subscriptions, validation and adapter routing.
Applications must not copy a runtime implementation to use the platform.
See [the proposed developer API](backend-functions.md) for code samples and the
confirmed first-release choice of backend-defined functions, with declarative local
reads deferred as a possible additional capability. Fullstack frameworks and serverless application
callers must fit the same authoring model. Public vocabulary is provisional:
database primitives should use select/insert/update/delete, while registered
functions use query/mutation to retain explicit read-only or transactional
execution semantics. Use `query({ input?, output?, handler, ...options })` and
the equivalent `mutation(...)` declaration, with `ctx` first and validated input
second in the handler. `query(handler)` and `mutation(handler)` are exactly
shorthand for `{ handler }`. Both function kinds accept optional validated input.
Output types are inferred from the complete handler return expression, including
derived expressions; generated runtime validation checks supported serializable
return values. An
optional explicit output schema adds compile-time and runtime checks. Retry option
names and numerical limits remain open; examples are illustrative. Generated bindings
should be directly importable and callable as `pending()` or `create(input)`, with
query subscriptions on the function itself. Ordinary DB use should not require constructing
a client tree or using a run method. Browser bindings use an application-scoped SDK
runtime; ordinary server database access uses server configuration and explicit
transaction handles, without requiring end-user auth context. Generated browser imports are the initial safe boundary;
imports from an authored declaration module would require a verified browser/server
build transform.

Client operation decision confirmed 13 October 2026: calls return awaitable
handles with execution state, data and errors; non-awaited handle use remains
supported. Awaiting obtains one authoritative result and does not keep a query
live. Query `.subscribe(input, listener)` (or `.subscribe(listener)` without input)
and handle subscriptions deliver full state snapshots and keep queries observed.
Optional framework bindings manage observation and rendering. A mutation handle
tracks one invocation; `mutation.optimistic(input, predictedOutput)` supplies
provisional result data without automatically updating unrelated query caches.
The proposed `.result` property and query `.watch` spelling are superseded in the
target client API. Existing implementation APIs are not yet migrated. Snapshots
keep execution `state` (pending/success/error) separate from `isStale` freshness
and `isOptimistic` prediction indicators; metadata changes also notify observers.
Auth scope changes and authorization loss clear protected results. Previously
observed queries retain their live subscription for five idle minutes by default,
configurable and subject to bounded idle-cache capacity. A returning observer
cancels expiration. Expiry releases the subscription and evicts its unused cached
result; await-only calls do not start background live retention. Auth changes and
authorization loss bypass this timer. Exact capacity and option spelling remain
implementation proposals.

Mutation completion decision confirmed 13 October 2026: resolve awaited mutation
calls and mark their execution successful on server commit confirmation with the
authoritative result. Do not wait for affected live subscriptions. Reconcile
optimistic query-cache contributions separately against committed query revisions;
commit acknowledgement alone must not discard those contributions and expose
older query data while subscription delivery catches up.

Connectivity decision confirmed 13 October 2026: v1 mutations require online
submission; no durable queue accepts new offline writes. Preserve original-request
recovery for already-submitted mutations with uncertain outcomes. Network failure
does not prove rollback, and browser online status does not prove server
reachability. Automatic live-query reconnect and authorized stale-result retention
remain supported; offline editing is outside the v1 scope.

Pagination boundary confirmed 13 October 2026: ordinary query handlers explicitly
choose database pagination and fixed settings or author-declared pagination input.
No automatic invocation pagination options, `ctx.pagination`, query declaration
pagination flag or separate paginated-query kind. Independent `.limit(n)` returns
bounded rows; explicit `.paginate({ limit, cursor })` returns rows and continuation
metadata, with the cursor omitted for the first page. Name the returned
continuation field `cursor`, not `nextCursor`. No separate `.cursor()`
builder method for v1. `.limit()` and `.paginate()` are terminal operations whose
return types omit query-builder methods; chaining them in either order is rejected
by TypeScript and must also fail clearly at runtime. Live pagination preserves
cursor boundaries; its limit sets the initial size, and observed pages may grow
or shrink. Standalone `.limit(n)` remains a strict maximum. Coordinate related
page updates when rows move between ranges; stable boundaries do not freeze row
membership. Default to primary-key order; honor author-selected ordering and use
the primary key as the final tie-breaker when needed. V1 indexes are explicitly
declared in the table schema and applied through schema deployment; the database
planner selects indexes automatically. Do not create indexes during query
execution. Index-generation tooling is deferred beyond v1. Declare indexes with
typed `.index(["projectId", "createdAt"])` chains on tables; generate names
deterministically unless `{ name: "..." }` is supplied as the second argument.
Advanced index options, exact metadata, client coordination and resource bounds
remain open.
Allow unindexed queries within execution limits; warn in the development console
when execution approaches a configured limit. Exact defaults and measurement
semantics remain to be settled. Configure defaults in `defineConfig` with partial
overrides on final query/mutation declarations only, never `.extend()` or browser
call input. Server execution timing excludes request upload and response delivery;
transport deadlines and mutation outcome recovery remain separate.
Default to independent 16 MiB limits for uncompressed serialized function input
and public output, configurable globally and per final declaration. Check input
before validation/execution and output after output transformation, before
mutation commit. Transport caps must accommodate these settings. Other numeric
execution-budget defaults remain provisional.

Reference/read-composition decision confirmed 13 October 2026: target `v.id("table")`
and `v.id(tableDefinition)` for typed primary-key references enforced as real
foreign keys. Both support the same options and generated ID types; strings avoid
circular initialization/inference and are validated against the assembled schema.
Resolve object references by registered table identity. Compose ordinary
reads and helpers for related data; no `.include()`, `one`/`many`, or named
relationship declarations in v1. Many-to-many links remain explicit tables.
Deletion restricts referenced rows by default, with explicit per-reference cascade
and set-null policies; set-null requires nullable fields. Cascaded changes must
share transaction atomicity and participate in managed reactive invalidation and
accounting. Efficient batch-read syntax remains to be settled.

Naming/registration confirmed 13 October 2026: retain explicit `defineSchema`
registration; its object keys are authoritative database table names, preserving
supported casing and spaces independently of TypeScript variable names. No
separate `tableName` option or automatic table-discovery fallback. Object
references resolve through registered identity; string references and database
calls use the same schema-key/database name. Tables can be defined in separate
modules and imported into the schema registry.

Coordinated publication target confirmed 13 October 2026, subject to a bounded
implementation prototype: active observed queries within one client runtime,
database source and auth scope publish a consistent database state together.
Mutation completion still waits only for confirmed commit. Existing independent
query delivery does not implement this guarantee. Validate shared-snapshot rounds,
atomic client publication, cancellation, auth fencing and reconnects. Retain the
previous authorized coherent view as stale on temporary connection/infrastructure
failure; ordinary handler errors clear that query's data by default and publish
an error alongside the round's other results. Auth/access loss clears affected
data immediately. `{ keepPreviousDataOnError: true }` may retain
the same input/auth scope's previous successful data alongside an error, explicitly
stale and outside the current batch's consistency guarantee. Support a global
default in `defineConfig` and per-subscription overrides; the package default
clears data on handler error. Auth/access failures always clear affected data.
Ordinary handler errors keep subscriptions active and recover on relevant data
changes, even after timed retries are exhausted. No separate recovery-on-change
toggle for v1. Configurable `retries` controls additional timed attempts; zero
disables timed attempts only. Retry defaults, eligibility and backoff remain open.
See the
[coordination contract](backend-functions.md#target-coordinated-live-query-publication).

Schema-validation decision confirmed 12 October 2026: retain `v` for database
schemas and use Standard Schema for function validators. Optional validation
adapters expose `createSelectSchema`, `createInsertSchema` and
`createUpdateSchema`, returning native library schemas with equivalent supported
declarative constraints. Tables expose type-only `$inferSelect`, `$inferInsert`
and `$inferUpdate` shortcuts. Database defaults remain database-applied; nullable
and optional remain separate. Native SQL checks are an advanced storage-only
escape hatch and cannot be automatically converted to function validators. See
[target validation contract](backend-functions.md#target-schema-validation-and-inferred-types)
for the agreed semantics. Extension input validators independently read the same
submitted object. Combine their disjoint parsed properties with the function's
parsed input into the second handler argument, called `input` in documentation.
Do not merge schemas; reject duplicate parsed keys rather than overwriting them.
Computed extension results add properties directly to `ctx`, with no locals
wrapper. Reject duplicate computed keys and reserved core context names, both
statically where detectable and at runtime. Generated validators never implicitly
coerce values; authors opt into library codecs, coercion or transformations.
These are target
APIs, not claims about the current implementation.

Decision confirmed 12 October 2026: support ordinary server-only database invocation
and a separately mounted query/mutation API for direct browser use. Server-only
applications own their endpoints, optionally through `@hulla/api`; they need not
declare client-facing functions for every database operation. Discovery registers
managed functions but does not itself expose network endpoints. Both modes share
one database engine. Do not introduce a DB `createClient` or client tree. Server
code uses normal direct database calls and `db.transaction(callback)`; exact
connection/bootstrap remains to be designed. Ordinary helpers provide reuse across
declarations and backend endpoints; declarations are not locally invoked by server
code or other declarations.

Keep `defineSchema` separate from `defineConfig`. Discover the authoritative schema
by project convention, allowing a path override for custom layouts rather than
requiring configuration to import the schema object. Generate the server database
entry point; applications need not hand-export a configured database instance.
Exact configuration-loading and generation syntax remain open.

Configure mount paths, transport/baseUrl and identity integration once through
application/framework setup. Same-origin browser calls need no URL argument;
remote deployments still require connection configuration. Imported functions do
not grant table/admin access, and the SDK must retire watches/cache state when
identity changes. An application using only mounted database declarations needs no
additional application API server. If one namespace is desired, optionally colocate the same DB bindings under the API entry
module; do not introduce a separate shared-client SDK or make composition mandatory.
DB owns live results and managed mutation recovery; ordinary HTTP operations keep
their API response/cache contracts.
`@hulla/api` is an optional hosting adapter; it is not a second route tree authors
must maintain.

User priority, confirmed on 5 October 2026: applications should feel immediate;
runtime responsiveness and client/server latency matter more than short builds.
Move reproducible schema/query preparation into compilation where appropriate,
while retaining authoritative runtime authorization, input validation, and database
constraints. A longer build is acceptable when it reduces shipped code or runtime
work without changing query semantics.

Decision, confirmed on 6 October 2026: PostgreSQL business rows remain authoritative.
Managed transactions commit rows, recovery receipts and synchronization revisions
atomically. The server polls those revisions and refreshes authorized PostgreSQL
queries; native function clients store results and pending mutation recovery state
in SQLite. The 12 October first-release read decision does not mandate a client
row replica or choose persistence defaults.
This supersedes the earlier mandatory WAL-capture architecture. The required path
has no full server row mirror or raw server event-history database. Capture and
exported-snapshot probes remain separate diagnostics. Streaming client bootstrap
separates working-memory budgets from total stored data.

Database integrity, concurrency protection and synchronization recovery take
priority over small latency improvements. Commands acknowledge only after commit
and do not wait for client visibility. Receipt revisions, source revisions and
query delivery cursors have distinct roles; a receipt is not a delivery event.
See [managed functions](managed-functions.md), [deployed adapters](sdk-adapters.md)
and [production operations](production-readiness.md) for these invariants and recovery.

Keep PostgreSQL drivers, diagnostic capture and server synchronization out of SDK dependencies.
Share schema, value and protocol semantics across the SDKs. A client row-query engine
and Wasm binding are not part of the selected server-result model. Do not retain a
parallel client query engine merely for possible future local-row support.

Separate interactive latency from synchronization work:

| Metric | Intended behavior and measurement |
| --- | --- |
| Delivered query result to rendered UI | Reuse current live results without a new loading state; measure result decoding, SDK publication and rendering, including allocations and payload size. |
| Cold query to usable result | Return a bounded authorized result promptly; measure realistic RTT, server work, transfer/decoding, and initial client application. |
| Write submission to durable acknowledgement | Authorize, validate, commit PostgreSQL rows and the request receipt, then return the committed result. Do not wait for client persistence or subscriber fan-out. |
| Commit to subscription visibility | Deliver authorized committed changes promptly through the sync path; measure polling delay, reevaluation, fan-out and client application separately. |

Committed and visible are distinct statuses. Preserve receipt recovery for ambiguous
outcomes and correlate committed operation results with sync delivery. An explicit
read-after-write request may need to wait for a visibility fence or read authoritative
state. A fast acknowledgement must not label a lagging query result current. Optimistic
UI remains optional per operation; retain prediction/reconciliation state until its
committed effects are represented rather than introducing a visual reversal while
subscriptions catch up.

The [current benchmark suites](benchmarks.md) measure command acknowledgement,
commit-to-client persistence, authorized serving and bounded queries separately.
They use the actual command table, with no shadow/mirror table. The internal native client workload does not measure network RTT, browser rendering,
large subscription fan-out or production tails. Use p50/p95/p99 under representative RTT,
concurrency, result sizes and subscriber counts as those layers are implemented.

Managed serving requires no replication privileges or WAL configuration. Its host
must poll durable revisions periodically and after reconnect; notifications are
optional wake-ups. Source identity, permission, revision and initial rows share a
snapshot. Tests cover bootstrap overlap, missed wake-ups, restart, rollback and
revocation. The retained capture/snapshot probes have separate diagnostic tests
and operational limits. Local measurements do not establish browser performance.

## 3. Lessons from comparable systems

This comparison is based on the linked architecture documentation, not an implementation audit or performance benchmark. Product details can change; recheck relevant sources before adopting dependencies.

| System | Relevant architecture | Lesson for this platform |
| --- | --- | --- |
| Zero | Queries synchronize a partial client replica. Available local results are returned before server hydration; mutations can run optimistically and reconcile. | Query-driven loading, local query evaluation, and typed named operations. Keep the server's authorized query definition authoritative. |
| Convex | Server functions query a reactive database; clients subscribe to function results. | Integrated schema and operation tooling. Arbitrary server functions should remain server-executed rather than being assumed locally reproducible. |
| PowerSync | Source changes become reusable sync buckets and client SQLite data. Checkpoints establish consistent local state; local writes can use an upload queue. | Checkpointed publication, reusable sync work, and separation between source connectors and delivery. Its offline upload queue is unnecessary for our initial contract. |
| Electric Sync | Read-path synchronization through shapes; application writes use a separate backend. Shapes support row/column scoping and progressive loading. | Separate authoritative commands from replica maintenance. Make synchronization scope explicit and enforce it server-side. |
| InstantDB | A Postgres-backed triple store, local query evaluation, pending optimistic changes, and dependency-based server invalidation. | Query-aware client state and separate speculative layers. A triple-store persistence model is an alternative, not a requirement. |
| LiveStore | Ordered events are synchronized and materialized into local SQLite; pending events are rebased. | Reactive projections, ordered delivery, and atomic publication. Whole-application event sourcing and client event rebasing are unnecessary for the proposed contract. |

Electric Circuits is an additional source-review candidate: its repository describes a Rust engine that consumes Postgres logical replication and maintains live query results incrementally. Investigate its design before investing heavily in our own incremental query execution. Do not assume its maturity or benchmark results apply to this platform.

Sources are collected in section 17.

## 4. System boundaries

```mermaid
flowchart TB
    Schema["Schema and operation contracts"] --> Compiler["Schema compiler and code generation"]
    Compiler --> TS["TypeScript SDK and validators"]
    Compiler --> RS["Rust SDK and types"]
    Compiler --> DBDef["Storage mappings and schema installation"]
    TS <--> Protocol["Portable platform protocol"]
    RS <--> Protocol
    Protocol <--> Core["Rust core: transactions, authorized operations, subscriptions"]
    Core <--> Runtime["Application business logic runtimes"]
    Core <--> Storage["Storage adapter contract"]
    Storage <--> PG["Postgres adapter"]
    Storage <--> Future["Future SQLite-family adapters"]
    Storage --> Feed["Consistent snapshots and committed changes"]
    Feed --> Subscriptions["Subscription maintenance and delivery"]
    Subscriptions --> Protocol
```

These are module boundaries, not a requirement to deploy each box as a service. Start with a modular Rust service; isolate application JavaScript execution where needed. Split services only for demonstrated operational or scaling needs.

### Recommended module responsibilities

| Module | Owns | Must not depend on |
| --- | --- | --- |
| Schema/compiler | Logical types, projections, operation contracts, generated artifacts | A particular client's UI framework |
| Query model | Typed query representation, portable semantics, capability checks | Raw backend SQL as the public contract |
| Storage adapter | Query compilation, transactions, constraints, mappings, schema installation | Browser state or UI subscriptions |
| Change source | Snapshot boundary, ordered committed batches, resume/reset behavior | Client presentation choices |
| Reactive core | Membership, dependency tracking, authorized projections, result changes | Postgres LSN formats or SQLite internals |
| Protocol | Portable scoped messages, typed values, errors, request IDs, cursors | SDK implementation language |
| SDK live state | Scoped function results, speculative layers, subscriptions and recovery; row coverage only in the deferred replica design | Database credentials or privileged application code |
| Business logic runtime | Execution of application-defined operations through a constrained API | Unrestricted access to another tenant's resources |
| Cloud control plane | Provisioning, billing, backups, operational management | Mandatory participation in self-hosted deployments |

## 5. Type safety from tables to SDKs

The incremental [schema contract discussion](schema-contract.md) records accepted
decisions, implemented contracts, and remaining questions. The separate
[wire format](wire-format.md) describes the implemented lossless JSON record codec.

### Recommended source of truth

Let developers author the schema in Rust or TypeScript, using each language's native tooling. Compile the chosen source into one declarative, language-neutral internal contract describing:

- Tables, fields, logical types, nullability, identifiers, relationships, indexes, and constraints.
- Explicit client-visible projections and their access policies.
- Named queries and commands, including input/output types and structured errors.
- Supported backend capabilities; release compatibility is deferred to publication.

The exact syntax is open: a TypeScript builder API and Rust derive/attribute macros with explicit schema registration are candidates. The canonical intermediate representation is a generated compiler artifact, not a file developers must ordinarily author by hand. A neutral-file import/export path can exist for tools.

A project selects one authoritative schema entry point, in either language. A TypeScript-authored schema can generate a Rust SDK, and a Rust-authored schema can generate a TypeScript SDK. Developers must not maintain two independent definitions of the same tables. Support changing the authoring language only through an explicit transition that preserves stable schema identities and verifies equivalent contracts.

The compiler produces backend DDL, runtime validators, Rust models/query builders, TypeScript types/query builders, and operation descriptors. Command contracts should reference schema types instead of duplicating table field definitions.

Type safety is layered:

1. **Compile time:** invalid fields, mismatched identifiers, wrong argument types, and invalid projections fail in Rust or TypeScript.
2. **Runtime boundary:** validate every network request, response, and decoded persisted value. JavaScript callers and malicious clients do not obey static types.
3. **Database:** enforce applicable foreign keys, uniqueness, nullability, and check constraints in storage.
4. **Deployment:** validate the installed storage contract and generated artifacts before serving traffic.

Static types do not prove authorization, cross-row business invariants, or absence of storage drift.

### Cross-language type rules

| Concern | Recommended contract |
| --- | --- |
| Entity identifiers | Nominal/branded IDs in TypeScript and typed newtypes in Rust; references point to the correct entity type. |
| Missing vs. null | Nullable fields still require presence on insert unless an explicit default is configured. Complete rows contain every field. Distinguish an omitted update field from explicitly setting a nullable field to null; insert defaults do not apply to omitted update fields. Rust patch types need an explicit unchanged/set/clear representation. |
| Integers | Define ranges. Encode 64-bit values losslessly, for example as tagged decimal strings on the wire; never silently pass them through an unsafe JavaScript number. |
| Decimals | Define precision/scale and exact encoding. Avoid implicit float conversion. |
| Time | Specify UTC instant encoding and precision separately from calendar dates. |
| Enumerations | Version explicitly; define handling for unknown variants during rolling upgrades. |
| Projections | Generate a projection-specific type. A partially synchronized record must not pretend to be a complete table row. |
| Generated values | Insert input types omit server-generated fields where appropriate; committed output includes their authoritative values. |
| Query results | Derive selected fields, nullable relationships, and aggregate result types from the query contract. |

Start with a narrow set of portable server query operators whose semantics can be made identical across adapters. Equivalent local execution is a requirement only if the deferred local-query capability is pursued. Specify text comparison, null ordering, numeric comparisons, and stable ordering with a unique tie-breaker. Reject unsupported capabilities explicitly.

### Alternatives

| Choice | Benefits | Costs |
| --- | --- | --- |
| **Recommended: Rust or TypeScript authoring → shared canonical representation → both SDKs** | Native editor support and composition in either language; a single source per project | Two authoring frontends need semantic parity and compiler conformance tests |
| TypeScript-only schema DSL → canonical representation | Familiar web application ergonomics and inference; fewer compiler frontends | Rust-only teams need a JS toolchain for schema authoring |
| Rust-only schema DSL/macros → canonical representation | Strong Rust tooling and compile-time checks; fewer compiler frontends | TypeScript-only teams need a Rust toolchain for schema authoring |
| Neutral schema file → canonical representation | Language-independent source and simple external tooling | Requires separate syntax/editor support and weakens native-language ergonomics |
| Database introspection as the source | Fits existing databases and external SQL workflows | Cannot infer authorization, optimistic behavior, operation contracts, or all semantic types |

This recommendation supersedes the initial preference for a hand-authored neutral schema file. It remains a proposal, not a settled public API choice.

### Evidence from other libraries

SDK support and authoritative schema authoring are distinct capabilities. A language-specific schema can describe the backend database, a client replica, or only serialization/types. The reviewed examples do not establish that most systems offer authoritative schema authoring in every client SDK.

| Library | Authoring model | Relevant distinction |
| --- | --- | --- |
| [Convex](https://docs.convex.dev/database/schemas) | TypeScript `defineSchema` / `defineTable` | Its [Rust client](https://docs.convex.dev/client/rust) does not change the documented TypeScript schema-authoring workflow. |
| [Zero](https://zero.rocicorp.dev/docs/schema) | TypeScript schema builder, often generated from Drizzle/Prisma | The Zero schema describes query types and relationships; the backend database schema remains a separate concern. |
| [InstantDB](https://www.instantdb.com/docs/modeling-data) | TypeScript `i.schema` / `i.entity` | Schema-as-code supports deployment and typed application queries. |
| [PowerSync](https://docs.powersync.com/intro/setup-guide) | Native client schema APIs for TS, Rust, Dart, Swift, Kotlin, and .NET | These describe the local SQLite replica, usually derived from backend schema and sync streams; they are not the authoritative backend migration model. |
| [LiveStore](https://docs.livestore.dev/api/livestore/livestore/namespaces/state/namespaces/sqlite/functions/table/) | TypeScript SQLite table definitions or Effect Schema | Models local materialized state alongside its event model. |
| [SpacetimeDB](https://spacetimedb.com/docs/tables/) | Table definitions in module code, including Rust and TypeScript | Closest authoring reference: choose a module language and [generate typed bindings](https://spacetimedb.com/docs/clients/codegen/) for clients in other languages. |
| [Prisma](https://docs.prisma.io/docs/orm/prisma-schema/overview) | Dedicated Prisma Schema Language | Counterexample to mandatory native-language authoring; one schema drives client generation and migrations. |

### Authoring compiler requirements

- Both frontends must produce equivalent canonical contracts for equivalent schemas, including defaults, field identities, relationships, constraints, projections, and portable validators.
- TypeScript types are erased at runtime: use executable schema descriptors, not a plain interface as the complete runtime schema. Rust macros must likewise emit explicit metadata; serialization traits alone do not describe the database contract.
- Preserve source locations through compilation so migration and validation diagnostics point to the authored Rust or TypeScript definition.
- Keep schema construction deterministic. Treat schemas as declarations with explicit build inputs; avoid silently changing deployed structure based on ambient environment, time, or network responses.
- Portable constraints and policies need a shared declarative representation. Arbitrary Rust closures or TypeScript callbacks cannot be translated automatically; business-specific validators execute in a declared server runtime and are not represented as portable static constraints.
- Generate client-facing types only for their intended public contract; schema authoring in a shared language must not bundle secrets or privileged server code into a client.
- Native SDK ergonomics do not require identical syntax. Verify semantic parity with shared fixtures and cross-language generation tests.

## 6. Rust and TypeScript SDKs

Both SDKs should support typed server queries and commands, function-result subscriptions, structured errors, authentication, commit/visibility status and optional optimistic behavior. Local row replicas are deferred possible capabilities, not first-release SDK requirements. Backend services and command-line tools can use the same remote function contracts.

Browser, native, and backend environments may have different transports and persistence adapters. A Rust client is not automatically privileged: authorization is determined by its credentials and endpoint, just as for TypeScript.

Recommended implementation direction:

- Share schema, query, value, and protocol definitions through code generation.
- Keep TypeScript ergonomics and framework integration in TypeScript.
- SQLite durable client materialization is implemented for the native task slice in `hulladb_client`. Browser/Wasm row storage integration remains unimplemented and deferred; if pursued, it must demonstrate equivalent transaction and coverage semantics. First-release persistence defaults remain open.
- The [Rust authoring facade](rust-authoring.md) provides registered callables through embedded and [remote HTTP/SSE runtimes](rust-remote-client.md), with client-only bindings and durable SQLite function intents. The shared native connector verifies certificate chains and DNS/IP identity, with configurable trust roots and loopback-only plaintext. The [TypeScript function adapter](sdk-adapters.md) provides generated callables and live browser results.
- Keep React bindings outside the core SDK; additional frameworks consume the same subscription interface.

The agreed [Rust authoring and call direction](backend-functions.md#rust-authoring-and-direct-imports)
uses table/input derives, query/mutation attributes and generated direct async
function imports. Queries can expose a same-named watch namespace such as
`pending::watch()`. Rust contexts remain snapshot/transaction typed and callers
resolve an established application/request scope. These are implemented in the
[Rust authoring facade](rust-authoring.md), with an embedded PostgreSQL runtime
and a [remote HTTP/SSE runtime](rust-remote-client.md) using client-only bindings
and SQLite recovery. Cross-language signature generation remains work.

This preserves shared semantics without requiring a client row-query engine for the first release. IndexedDB function-result and mutation-intent persistence is implemented; the shared Wasm local row/query replica remains unimplemented and deferred. These implementations do not settle persistence defaults. There is no measured claim that Wasm outperforms TS.

## 7. Write execution and optimistic behavior

### Two supported behaviors

Public names such as `optimistic`, `server`, or `mode` are placeholders. The required behavior is:

| Behavior | Client before confirmation | Server | Completion |
| --- | --- | --- | --- |
| Server-confirmed | Exposes pending status; confirmed data stays unchanged | Authorizes, validates, executes transaction | Success only after commit |
| Optimistic local + server | Adds a speculative layer to visible function results | Runs the same authoritative operation | Reconciles on commit; removes failed speculation on rejection |

Provide operation-level defaults and optional call-level overrides. Server-confirmed is the recommended initial default. Optimistic execution is available only when a valid local prediction is defined; selecting it for an unsupported operation must fail clearly rather than guess.

### Client state model

**Visible state = confirmed function results + ordered pending optimistic layers.**

For existing native row replicas, the analogous design layers predictions over
confirmed rows; native optimistic presentation remains future work. The typed public
optimism API remains open.

Each pending operation has a stable request ID, arguments, dependencies, and status. Applying a local prediction must not modify the confirmed base.

On a definitive rejection, remove the failed layer and reevaluate remaining layers against the latest confirmed state. Never roll back by restoring an old database snapshot. If later predictions depend on the failed operation, explicitly invalidate or recompute them; a dependent command must not quietly acquire a different meaning.

On commit, replace the prediction with authoritative data. Correlate receipts and sync delivery so the prediction is not removed before its committed effects are visible. Support the server canonicalizing values or producing a different result from the prediction.

The TypeScript HTTP adapter now implements ordered predictions over exact cached
function results, rejection rebasing, explicit dependency invalidation, and receipt
visibility reconciliation. Confirmed results stay separate from speculation.
See [implemented behavior and limits](concurrency-and-optimism.md). Native client
optimistic presentation and a portable declarative table format remain subsequent
work.

### Prediction implementations

Typed function-result prediction APIs remain open. Custom operations may use
language-specific pure optimistic updates or require server-confirmed execution.

A predictor can only use available client data. It has no access to secrets or external side effects. IDs and other inputs needed for stable prediction should be explicit; predicted server-generated values must remain provisional.

Predictor code is a UI behavior, never an authorization mechanism. Clients do not submit trusted row changes for the server to blindly apply.

### Network failures and concurrency

- Reject new mutation authoring when disconnected; draft form state remains an application concern.
- A lost response after submission means **outcome unknown**, not failure.
- Recover submitted operations using the same request ID or a status lookup; never retry under a new ID automatically.
- Persist minimal request recovery metadata when necessary. This is distinct from offering an offline mutation authoring queue.
- Scope idempotency to the application/principal and operation payload; reusing an ID with a different request must fail.
- Managed receipt retention supports explicit bounded input/result compaction with permanent request markers. Expired recovery is terminal and cannot silently resubmit a committed request; see [operational visibility](operations-visibility.md).
- Preserve existing strict UPDATE/DELETE missing-row semantics; strict UPDATE
  fails when the target is absent. Ordinary UPDATE is not an upsert. An authored
  mutation may explicitly recreate a deleted row atomically with a stable key,
  required insert data and applicable defaults, subject to normal constraints and
  authorization.
- Optional expected-version or base-value preconditions protect edits based on previously read state; ordinary CRUD does not require caller-supplied row versions.
- Semantic commands such as increment execute against authoritative state inside the transaction.

### Application concurrency policies

Target contract, confirmed 12 October 2026: restricted declared mutations use
PostgreSQL serializable transactions and automatically retry the whole operation
on retryable conflicts after confirmed rollback, within a bounded budget. The
executor enforces replay restrictions; external effects belong in ordinary
application backend/API code outside the mutation.
These defaults do not require caller-supplied row versions for ordinary CRUD.
Optional original-version or base-value preconditions protect stale UI intent.
The exact public API spelling and options remain open. This is the target contract;
the current generic runtime still exposes serializable execution as opt-in.
Ordinary server `db.transaction(callback)` callbacks run once by default and surface
transaction conflicts rather than automatically replay application code. They retain
the same constraints, atomic commits, revisions and reactive invalidation.

Application conflict rules remain distinct from isolation, resource scheduling,
and sync ordering. For example, if one mutation deletes a row and another updates
it, ordinary `update` retains strict missing-row failure. The developer explicitly
chooses `upsert` to recreate it with a stable key, required insert data and applicable
defaults (confirmed 12 October 2026; exact arguments remain open). Such behavior is
atomic and subject to all constraints and authorization checks. It follows the
chosen serialization order: a later delete can still remove a recreated row, so
this is not an unconditional
update-wins guarantee. Do not add tombstones or CRDT behavior to ordinary rows.

Recommend safe defaults based on the operation's meaning rather than one policy
for all writes:

| Operation / policy | Proposed behavior | Tradeoff / requirement |
| --- | --- | --- |
| Declared CRUD mutation | Run atomically under serializable isolation and retry the whole restricted operation on retryable conflicts; caller row versions are not required | Optional original-version/base-value preconditions reject stale UI intent; retries follow database serialization order |
| Semantic command such as increment or conditional reservation | Evaluate against authoritative state atomically, preserving declared preconditions | Avoids stale read/replace cycles; concurrent updates to the same resource still contend |
| Explicit overwrite | Apply the specified fields to current state; later server serialization wins on overlapping fields | Deliberately permits replacement of another edit; client clocks do not determine the winner |
| Merge independent field edits | Accept only when touched fields still match their base values or field versions | Requires trustworthy base comparisons/version metadata; fields with shared invariants cannot be merged independently |
| Explicit delete/update conflict behavior | Preserve strict missing-row failure, or author an atomic recreation with stable key and complete required insert data | Defaults, constraints and authorization still apply; later serialized deletes may remove the recreated row |
| Custom resolver | Backend handler decides whether to reject, merge, or recompute using authoritative current state and the submitted intent/preconditions | Runs within the transaction's concurrency protection; output is revalidated against permissions, constraints, and business invariants |
| Specialized collaborative data | Explicit CRDT or other domain-specific operation model | Requires its own convergence, validation, deletion, and bounded-storage contract; not the ordinary row default |

An application can establish defaults, but the registered backend operation owns
the effective policy. Clients cannot choose a weaker policy or bypass a required
precondition. Changes to these semantics must be versioned with the operation;
retrying an existing request identity must preserve its original meaning.

Custom conflict logic can read through the authoritative transaction API. Logic
in a declared mutation must be safe to rerun; ordinary server callbacks run once.
Materializers and client predictors remain deterministic; custom resolution executes
on the backend, not independently on each client.
Any generated values or request inputs needed for stable retry semantics must
have an explicit lifecycle. Applications own external side effects through their
backend/API routes or framework functions, and own their delivery/idempotency policy.

The optional implemented [transactional outbox](outbox.md) supplies durable enqueueing
for native and TypeScript mutations; it is an implementation capability, not an
action declaration or general job-runner requirement. Jobs commit with their data
and receipts; separate workers use fenced leases, bounded retries and stable job IDs.
Delivery is at least once, so external handlers must deduplicate those IDs. Restored jobs retain their old
source fence until an operator explicitly reconciles them.

### Isolation, retries, and contention

The target for restricted declared mutations is a PostgreSQL serializable
transaction with bounded automatic whole-operation OCC retries after confirmed
rollback. The executor enforces replay restrictions; a client cannot weaken them.
Ordinary server transaction callbacks run once by default. An OCC failure in any
helper work invalidates the entire transaction; retrying only a helper cannot
repair a run-once outer transaction. External effects execute in application
backend code outside restricted declarations.

ACID transactions, storage constraints, authorization and request idempotency
remain mandatory.

Keep internal transaction retries distinct from application conflicts. Retry
serialization failures or deadlocks only after rollback is confirmed and the
whole operation can safely be rerun. An optional expected-version/base-value
failure is an application conflict; do not replace its precondition with the
latest state and retry automatically. A custom operation may explicitly recompute
its intent against current state. Ambiguous commit outcomes use the existing
request identity and recovery contract.

For contention, coordinate the smallest scope needed by the operation: a row,
declared aggregate/resource, or the database's transaction conflict detection.
Multi-row invariants need transaction-wide protection; row versions alone do not
prevent write skew. Use stable lock ordering, deadlines, bounded queues, and
backpressure. Serialization of one hot resource is different from serializing
all unrelated writes in a workspace. An event log does not remove contention on
a shared resource or make business conflicts disappear.

The fixed command backend deliberately locks one workspace before every
command and supports only row-level expected-version edits/deletes. This is not
the production policy API. Managed revision tracking currently uses that same workspace lock. Any narrower
coordination must preserve atomic invalidation and request recovery; allocating
sequence IDs outside a transaction is not a commit-order proof. Delivery ordering
and duplicate-safe application remain separate from operation conflict policy.

The [generic managed function runtime](managed-functions.md) currently applies
conservative coordination to arbitrary declared tables: it locks one managed data
scope before authorization, handler reads/writes and receipt checks. Every
multi-operation mutation shares that transaction and advances the scope revision
once. Separate scopes proceed independently. Registration currently supports opt-in
serializable execution with bounded, explicitly replay-safe retries; this is
implementation status, while the target default is stated above. Handlers can
overlap in one scope; the final scope revision remains a contention point. See
[transaction concurrency and optimistic results](concurrency-and-optimism.md).

Benchmark independent records, a hot record, a hot aggregate, and multi-row
invariants separately. Record successful throughput, conflict/retry rates,
attempts per command, p95/p99 latency, lock wait, queue depth, and commit-to-client
lag. Use the current workspace-serialization benchmark;
choose defaults from representative correctness and workload evidence. The aim
to cover most applications is a product target, not a measured coverage claim.

## 8. Reactive server query results

### First-release read model

Use named parameterized backend query functions. The server executes each function
in an authorized consistent snapshot, sends its initial result and maintains the
subscription as committed data or permission inputs change. The client observes
the returned value; arbitrary TypeScript or Rust server callbacks are not executed
locally. Server database access begins with filters, projections, stable ordering
and bounded results; stage joins and aggregates according to correctness and
performance evidence.

| Alternative | When it fits | Tradeoff |
| --- | --- | --- |
| **Server-result subscriptions (first release)** | Named backend functions, including private computations and derived output | New arguments or filters require server evaluation; cached outputs do not provide arbitrary local querying |
| Query-driven local row synchronization (deferred) | Applications needing new covered queries without a network round trip | Requires read scopes, portable local execution and coverage proofs |
| Explicit collection/workspace synchronization (deferred) | Small, bounded working sets and rich local filtering | More data transfer and memory than a specific query requires |
| Whole database/store replication (deferred) | Small isolated documents or personal datasets | Poor default for multi-tenant applications and sensitive data |

The deferred alternatives are design options, not first-release acceptance work.

### Result identity, completeness and freshness

Track function identity, arguments, authorization scope, source identity, delivery
cursor and whether the result is complete for that invocation. A complete result
at a cursor is not proof that it is current now.

Cache presentation decision, confirmed on 12 October 2026:

- A cold query, including a fresh page load without an established live result,
  waits for the first authorized server result by default. A persisted result from
  a prior session is not displayed before server validation by default.
- Share and retain results for the same function, arguments and authorization
  identity while the SDK maintains their live subscription. Reusing such a result
  must not introduce another loading state or an unnecessary request.
- Component unmounting need not immediately discard a result. Retention must be
  bounded; exact limits remain implementation work. An entry retained for immediate
  fresh reuse must retain its live subscription too. Once that subscription stops,
  elapsed time alone cannot prove freshness; reactivation requires server validation.
- During a transient disconnect, keep an already-displayed result visible with
  stale/disconnected state and reconcile on reconnect. Retained data is not proof
  of freshness. A cold or reactivated stale query waits for validation by default.
- Account changes, sign-out and confirmed access revocation retire affected state.
  Results cannot be reused across authorization identities.

The SDK owns subscription updates and freshness tracking; ordinary callers do not
manage TTLs or manual invalidation. Disk persistence and an optional stale-result
display mode are separate design choices, not implied by retaining live results.
Durable mutation recovery remains independent of query-result display policy.

For top-N queries, deletes and ordering changes may require fetching a replacement
row. Query maintenance must track records entering the result, not only records
already delivered. Authorization dependencies are also query dependencies.

### Atomic publication

Snapshots must align with the subsequent change stream without gaps. Buffer data until its snapshot is complete, then publish atomically.

For live updates, publish a transaction's relevant changes atomically within an established subscription group. Define the group boundary explicitly; independently bootstrapping subscriptions are not automatically a single globally consistent view. Grouped subscriptions should be available for screens requiring a consistent cross-table view.

## 9. Authorization and sensitive data

Anything delivered to a client must be considered readable and copyable by that client. Local storage encryption and hiding UI fields do not substitute for server authorization.

### Ownership

Decision confirmed 12 October 2026: authentication is bring-your-own. Applications
connect an established identity provider or authentication library; neither Hulla
nor its application developers should need to build password, login or session
cryptography for this integration. Hulla does not supply account management, login
flows or its own identity provider. The mounted query/mutation endpoint delegates
credential verification to established implementations, with a trusted custom
server-verifier extension for existing systems. Ordinary server CRUD and transactions
do not obtain sessions or require an auth-bound database handle; application backend
code owns authentication and authorization there. Exact endpoint verification syntax
and initial provider coverage remain open.

Product constraint confirmed 12 October 2026: authenticated production use must
have a fully self-hosted, open-source path with no mandatory paid auth service or
provider account. Hosted providers are optional integrations, not required runtime
dependencies. Validate and document a self-hosted integration as a first-class
release path. Better Auth is the recommended first documented integration, with an
optional storage adapter and schema-generation support. Better Auth owns its login
and session functions; @hulla/db does not generate a competing authentication system.
Storage integration does not by itself authenticate function calls or make auth
tables browser-accessible.

Browser authentication state is application-driven. The application observes its
existing auth library and calls a framework-independent `setAuth` API to update
status and cache scope. A scope-key change for a user/session/tenant switch clears
the previous scope's results and fences late responses; old mutation intents must
never replay as a different caller. Credential transport is configured separately:
`getToken` does not belong in `setAuth`. Cookies or an application-supplied authenticated
transport supply credentials for server verification. Browser status and scope keys
are lifecycle signals, never identity proofs. Exact state variants and transport
configuration remain open. No mandatory browser auth-provider component or automatic
framework/provider-pair adapter is required.

The live runtime owns subscription reauthentication, cache isolation and expiration
enforcement. A cookie change does not alter an already-open stream: reauthenticate
or reconnect using current credentials. Server-side expiry and revocation handling
cannot rely on client notifications. Provider-specific revalidation windows must be
documented; shared auth storage only enables prompt session invalidation when an
explicit session-to-subscription dependency integration is implemented.

Expose the application's verified, typed identity through `ctx.identity`. Browser
payloads cannot supply a trusted user/tenant identity. The application owns roles
and business access rules. Hulla owns the integrity of the verification boundary,
transaction/snapshot execution, per-identity result isolation and live-delivery
enforcement, including configured credential/session expiry and revocation handling.
It does not impose a membership schema.

Decision confirmed 12 October 2026: ordinary function code is the default
authorization model. Authors use the verified identity, database reads and ordinary
conditionals, with reusable helpers where useful. Checks protecting database
invariants execute in the same snapshot/transaction as the protected operation.
No RLS, table-level policy language or separate native policy implementation is
required to author an ordinary application. Optional row/table-level enforcement
may supplement handlers as an advanced capability. Hulla must still track database
reads made by authorization helpers for live invalidation and must not reuse live
query results without the applicable authorization checks. Decision confirmed
17 October 2026: short-lived mutation confirmation delivers the original authorized
response to the same currently verified caller, tenant and authentication scope.
Database permission changes after commit do not revoke that historical response;
identity/scope changes and invalid, expired or revoked credentials block delivery.
Confirmation does not replay application guards. See the
[SDK confirmation contract](sdk-architecture.md#optimism-and-mutation-confirmation).

Decision confirmed 12 October 2026: public functions accept anonymous callers by
default. Absent credentials produce `ctx.identity === null`; valid credentials
produce the verified application-typed identity. Invalid or expired credentials
produce an authentication error rather than silently becoming anonymous. Functions
that require sign-in check explicitly in ordinary code. There is no required
anonymous-access option on declarations.

Provide room for optional reusable identity/permission helpers and typed declaration
extensions so applications can define their own authenticated or authorized variants
of query and mutation. `.extend({ input?, handler })` and callback shorthand return
a new declarer with validated shared inputs and typed, invocation-scoped additions
directly on `ctx`. The extension handler receives context first and validated
`input` second; chained extensions see earlier computed context. Reject duplicate
computed keys and reserved core names such as `db` and `identity`. Extension
handlers execute sequentially in declaration order before the final handler;
failure stops subsequent execution and follows the enclosing rollback policy.
Validate each stage immediately before its handler. Each extension sees validated
input accumulated through its stage. Independently parsed extension and function
inputs combine into the second argument, with duplicate parsed keys rejected
rather than overwritten. Output validation/transformation precedes mutation commit.
Per-stage validation does not introduce additional client/server round trips.
Shorthand functions retain
inherited input validation and receive the inherited parsed input. Extensions preserve the declaration
kind, validation, snapshot/transaction boundary and retry restrictions. Database
reads in guards are tracked and database-dependent guards run with the protected work.

Checks that depend on current data run inside the same transaction or read snapshot
as the operation. Long-lived subscriptions retain the application's authorization
scope, recheck policy dependencies and terminate when access is revoked. Initial
endpoint authentication alone cannot authorize all later deliveries. The
[function host](sdk-adapters.md) accepts application-owned authentication and
rechecks authorization before delivering query results.

### Required boundaries

1. **Authentication:** derive identity from verified credentials, not client-supplied user or tenant IDs.
2. **Read authorization:** enforce allowed entities, rows, and relationships before constructing snapshots or updates.
3. **Field projection:** define explicit client-visible fields. Adding a database column must not silently add it to client delivery.
4. **Queryable fields:** restrict filtering, ordering, and aggregation over private fields to prevent unintended inference.
5. **Write authorization:** validate the authoritative command and current state inside the transaction where required.

Named public queries resolve to server-controlled definitions. Client arguments may narrow permitted data but cannot remove authorization predicates. Database row-level security may provide defense in depth; logical replication and service credentials must not be assumed to apply end-user policies automatically.

Apply equivalent rules to mutation responses, error details, prefetches, reconnects, cached delivery, and server-result queries. Tenant and authorization scope must be part of cache/subscription sharing keys.

When membership or permissions change, reevaluate affected subscriptions, stop unauthorized delivery, and remove affected data from cooperating clients. Do not rely only on the protected row changing. Cache namespaces must isolate applications, accounts, and source identities; logout must invalidate relevant data and in-flight responses.

Provide persistence controls for authorized but sensitive data, including memory-only storage. These reduce retention; they cannot revoke data already observed or copied. Private backend records should remain entirely server-side, with only approved derived results returned where appropriate.

## 10. Storage adapters and managed synchronization

### Adapter contract

A storage backend must provide more than SQL execution:

- Logical-to-physical type mappings, query compilation and capability reporting.
- Transactions, storage constraints, concurrency protection and normalized errors.
- Schema installation; release migrations are deferred until publication.
- Atomic managed invalidation revisions with data, policy and request receipts.
- Consistent authorized snapshots paired with source identity and revision.
- Recovery, duplicate handling and explicit source-reset semantics.

Keep backend driver types out of the portable delivery protocol. SQLite, libSQL
and Turso adapters must demonstrate equivalent invariants rather than inheriting
claims from SQL compatibility. General handlers need serializable behavior or
operation-specific equivalent locking; row versions alone do not prevent write skew.

### Selected change source

Managed transactions are the sole production write/change-tracking path. The
schema-driven PostgreSQL runtime commits rows, scoped revisions and recovery
receipts together. Failed tracking or receipt writes roll back the mutation.
[Concurrency policies](concurrency-and-optimism.md) define isolation, retries and
conflict protection for registered functions and direct transactions.

The [function host](sdk-adapters.md) polls durable revisions and evaluates
authorized snapshots. Results and their revisions share one database snapshot.
Identical scoped queries share evaluations; unchanged revisions can reuse results
after authorization is rechecked. Top-N queries are reevaluated to discover newly
matching rows. Deliveries are individually consistent, not a global multi-query
snapshot. Notifications, if added, only wake this recovery loop.

Receipts retain their commit source identity. Delivered snapshots carry the source
and revision paired with their result, allowing mutation visibility checks even
when filtering leaves a result unchanged. The [Rust remote client](rust-remote-client.md)
atomically persists function results and same-request recovery state in SQLite.
The browser SDK uses in-memory live results. Local row replicas remain deferred.

### Write boundary, administration and diagnostics

Application/client roles must not receive direct SQL DML access or runtime database
credentials. The trusted managed service owns application writes; schema installation and
maintenance use a separate owner role. Managed role provisioning now creates
dedicated owner/runtime/backup groups; durable quotas and typed admin callbacks
retain the managed transaction boundary. See [production operations](production-readiness.md). General handler/trigger/cascade effects must be
tracked before they are supported. Arbitrary independent SQL writers are outside
this contract and must not be silently advertised as synchronized.

Initialization creates the current schema and a durable deployment UUID.
Incompatible pre-publication fixtures require explicit recreation. Restore, source replacement or
out-of-band writes require controlled maintenance, integrity validation, a rotated
source UUID and fresh subscriptions. Backups copy source identity, so connection
identity alone is insufficient. Revision regression or an observed source change
closes existing lifecycles. See [managed functions](managed-functions.md) and
[production operations](production-readiness.md).

`hulladb_cdc` and its [bounded raw history](cdc-slice.md) remain diagnostic modules.
The [snapshot/mirror feature](cdc-snapshot.md) exercises exported-snapshot handoff;
its privileged values never become SDK data. These modules are not dependencies of
managed serving. A future external-writer CDC connector requires a separate design
and cannot quietly become a second competing source for managed data.

The [Rust remote client](rust-remote-client.md) stores function results in SQLite.
The [supplied browser service](browser-service.md) serves generated declarations
with application-owned authentication.

## 11. Subscription execution strategy

Recommended v1: track query dependencies conservatively, rerun affected queries after commits, and send result differences. Dependencies must include potential new matches and authorization inputs, not merely rows returned last time.

Share execution where the query, arguments, authorization scope, and schema semantics are identical. Bound result sizes, subscriptions, memory, and per-tenant work. Coalesce safe updates and impose backpressure; a slow client may need a fresh snapshot rather than an unlimited backlog.

The [generic function host](sdk-adapters.md) now shares evaluations for exact query
and identity matches within each process. Polls reauthorize and skip the handler
when its scope revision is unchanged; each stream retains its own credential
checks and delivery sequence. New subscribers recheck policy immediately, and the
last disconnect releases the cached query. Any scope write still causes full
reevaluation; table-level invalidation and coordination across hosts remain work.

Alternative: incremental view maintenance updates results directly from changes. It can reduce repeated query work but makes joins, aggregates, limits, and permission changes substantially harder. Preserve a replaceable execution boundary and adopt incrementality after measuring representative workloads.

## 12. Server business logic

Both SDK languages should invoke the same typed operations. SDK availability does not require every operation implementation to execute in both languages.

Recommended runtime boundary: the Rust host owns transactions and exposes a constrained transaction/query API. Rust handlers can run in a trusted self-hosted process; TypeScript handlers can run in a separate JavaScript runtime process. Managed hosting must isolate customer code and enforce resource limits.

| Runtime option | Benefit | Cost / limitation |
| --- | --- | --- |
| **Rust host + separate JS worker process** | Clear ownership and familiar TypeScript execution | IPC overhead and another runtime to package |
| Embedded JS engine | Tighter integration and potentially less IPC | Sandbox, resource limits, bindings, and engine maintenance |
| Application-hosted handlers | Works with existing backends and languages | Distributed transaction API design and more deployment coordination |
| Declarative operations only | Portable and easy to validate | Insufficient for general business logic by itself |

Keep the core sync path independent of Node.js. A deployment using only supported declarative operations or Rust handlers should not need a JS worker. Do not hold database transactions open across arbitrary external API calls.

Fullstack applications can keep schema, function and UI source in one repository.
Generation must enforce separate browser-safe callable bindings/types and server-only
handlers, drivers and admin bindings. SSR/loaders and application backends can use
the server-only database directly, including `db.transaction(callback)`, under
explicit server configuration. They own user-facing authorization and external-service
orchestration. Shared ordinary helpers accept the caller's database handle; no local
server-to-declaration invocation or auth-binding API is part of the target. Helpers
used in declarations preserve their validation boundary, tracked snapshot/transaction
execution and restricted runtime guarantees.

Serverless application callers can use the server database connection or the
mounted function HTTP boundary against the managed service when explicitly acting
as a remote caller; this is not a local declaration-composition API. Live
subscription coordination and durable revision polling must outlive individual
framework requests. Executing application-hosted handlers within an
ephemeral invocation requires a separately designed transaction bridge, including
ownership, cancellation and disconnect rollback. A fully serverless database/sync
runtime additionally needs durable/shared coordination and a supported live
transport; the current in-process scheduler/feed does not provide that deployment.
See [fullstack and deployment details](backend-functions.md#fullstack-frameworks-and-serverless-applications).

The [intended developer API](backend-functions.md)
requires schema-typed database contexts, registered functions and generated
client/admin bindings. The native [managed function runtime](managed-functions.md) now supplies a
schema-driven transaction API, registered Rust handlers and tracked reads.
The [deployed adapters](sdk-adapters.md) now connect generated TypeScript callables
to registered native functions. The [Rust authoring facade](rust-authoring.md) adds
typed contexts and registry adapters. [Production operations](production-readiness.md)
adds request scopes and typed admin access. The optional TypeScript worker executes
callbacks through managed transactions; enforcing the restricted executor and
providing the ordinary server database API remain work. A generic facade over the
task enum is insufficient.

## 13. Schema evolution and protocol compatibility

### Pre-publication development

Until the packages are published to Cargo and npm, implement the current contract
only. Do not carry schema generations, wire/file-format version checks, upgrade
migrations, legacy readers, or migration journals for unreleased prototypes.
Recreate incompatible development databases and SQLite fixtures explicitly;
initialization must never silently delete or rebuild existing data.

A future development watcher may validate the authoritative schema, regenerate
Rust/TypeScript types and validators, and rebuild affected handlers. Generated
artifacts should be published together only after successful validation and builds.
Changing a storage contract requires an explicit fixture reset during this phase,
not automatic data conversion or a candidate/active version registry.

Source and subscription UUIDs, monotonic revisions, and row concurrency versions
remain required for live correctness. They detect replaced sources, reject late
responses, and prevent conflicting writes; they are not release compatibility tags.

### Publication boundary

Before the first Cargo/npm release, define protocol compatibility and supported
upgrade paths for persisted server/client data. That work must specify inspectable
migration plans, rollback/recovery guarantees, ownership privileges, and deliberate
execution of destructive changes. Introduce version negotiation, migration journals,
and cross-version deployment handling at that boundary, when they protect actual
users and persisted application data. They are not current implementation goals.

## 14. Self-hosting and managed cloud

Self-hosting should include the same core database, synchronization, SDK, authorization, and optimistic behavior. It must run without an account on the managed service. Initially package a Rust service, Postgres, and an optional JS runtime using containers.

Licensing remains open: the product requirement is free self-hosting, while the exact license and permissions for competing hosting require a separate choice.

Managed hosting can charge for operations: provisioning, backups, recovery, upgrades, scaling, monitoring, and support. Keep its control plane separate from the data plane so self-hosted deployments remain independent.

| Provider option | Recommendation / tradeoff |
| --- | --- |
| **AWS: ECS/Fargate + RDS Postgres** | Provisional production-cloud recommendation for container isolation and managed database operations. Higher baseline cost and operational configuration may be unfavorable for tiny projects. |
| Fly.io + managed Postgres | Alternative for a smaller container-oriented launch. Validate service placement, availability, and costs before selection; local volumes do not automatically replicate. |
| General-purpose VMs / dedicated servers | Useful for self-hosting and potentially lower infrastructure spend; our team owns more recovery, failover, and capacity management. |

The provider is not an architectural dependency or an approved deployment choice. Estimate economics before committing: idle projects, persistent connections, query fan-out, transfer, backups, and application-code isolation all matter. Start with one write region per deployment; global multi-writer operation is outside v1.

## 15. Implementation sequence and acceptance gates

This sequence is guidance for future authorized implementation, not an instruction to begin implementation now.

Current implementation includes logical schemas and value codecs, the
[schema-driven PostgreSQL runtime](managed-functions.md), [Rust authoring](rust-authoring.md),
[TypeScript schema generation](schema-authoring.md), [direct database calls](server-database.md)
and [deployed HTTP/SSE adapters](sdk-adapters.md). The [Rust remote client](rust-remote-client.md)
provides client-only bindings and durable mutation recovery. The optional
[TypeScript worker](typescript-functions.md) executes authored callbacks through
managed transactions. The [diagnostic snapshot](cdc-snapshot.md) tests raw MVCC
handoff independently of serving.

The fixed task-specific prototype, replica, sync crate, host, examples and
benchmarks have been retired. These implementations do not establish completion
of all acceptance gates below. Local browser row synchronization remains deferred.

### Phase 1 — Contracts and semantics

Specify logical types, projection rules, exact function-result identity/completeness, operation lifecycle, errors, transaction consistency, snapshot boundaries, and protocol compatibility. Local row-query coverage is deferred. Build schema generation for Rust and TypeScript against a small representative schema, including watch mode and source-level diagnostics. Compatibility/versioning is deferred to publication.

**Gate:** both languages reject invalid fields, IDs, nullability, and command arguments; runtime validators reject equivalent invalid network payloads; generated storage constraints match the contract.

### Phase 2 — Authoritative vertical slice

Implement one Postgres-backed service, named commands, structured errors,
idempotency receipts, and managed revision-based authorized client materialization. Use the current schema directly; recreate incompatible development fixtures explicitly. Exercise native function-result storage
and recovery in adapter conformance tests before its storage contract hardens.

**Gate:** update/delete races, duplicate submissions, transaction rollback, commit ordering, and crash recovery behave according to the same adapter conformance tests.

### Phase 3 — Reactive reads

Implement server-function initial results, resumable result subscriptions, exact
invocation completeness and scoped live state. Deliver equivalent functionality in
both SDKs. Local row replicas, local-query coverage and the shared browser/Wasm query
engine are deferred possible capabilities, not gates for this release.

**Gate:** no missing updates across snapshot handoff, duplicate delivery, reconnect, cursor expiration, or restart; partial results are never marked complete.

### Phase 4 — Optimistic writes

Implement optional predictions, layered state, operation dependencies, reconciliation, and visibility tracking. Exercise server-confirmed and optimistic modes against the same server commands.

**Gate:** rejection cannot overwrite concurrent server changes; successful predictions reconcile without flicker or duplicate application; ambiguous outcomes remain recoverable.

### Phase 5 — Security, evolution, and operability

Policy invalidation, identity cleanup, durable scope quotas, principal admission
budgets, role provisioning, framework/admin integration, backup/restore tooling and
self-hosted containers are implemented. See [operations and recovery drills](production-readiness.md).
Evaluate representative deployment capacity, managed-cloud costs and runtime
isolation. Publication upgrade contracts remain deferred until publication.

**Gate:** unauthorized data never enters snapshots, deltas, receipts, or shared caches; incompatible clients fail clearly; recovery is demonstrated rather than assumed.

### Required scenario suite

| Scenario | Required result |
| --- | --- |
| Two clients edit the same version | Explicitly chosen concurrency policy; no accidental lost update hidden by speculation |
| Delete followed by stale update | Strict update fails; no implicit resurrection |
| Rejected optimistic write while remote updates arrive | Only failed speculation disappears; confirmed remote state survives |
| Command commits but response is lost | Same request ID recovers the existing result without duplicate effects |
| Multi-row transaction affects a subscribed screen | Atomic publication within its defined subscription group |
| Top-N row leaves the result | Correct replacement and ordering |
| Membership is revoked without changing target rows | Subscription invalidation and no further unauthorized delivery |
| Logout/account switch during a response | Old-principal data cannot populate the new cache |
| Permission differs between two subscribers | No cross-subscriber leakage through shared execution or cache keys |
| Snapshot races with writes | Exact handoff into the live stream without gaps |
| Rust and TS clients consume the same server query result | Equivalent decoded values, ordering, result completeness and subscription semantics |
| Save a valid schema change in development | Regenerate both SDKs and validate/build handlers; explicitly recreate incompatible storage fixtures before serving |
| Save an invalid schema or fail application compilation | Actionable diagnostics; previous compatible runtime remains active |
| Storage contract changes during pre-publication development | Explicit recreation of incompatible fixtures; existing data is never reset silently |
| Development watcher targets a production deployment | No implicit storage changes; explicit production deployment workflow required |
| Equivalent Rust-authored and TS-authored schemas | Equivalent normalized contracts, storage mappings, and generated client semantics |

Use property-based/differential tests for query and adapter semantics, and fault injection for retries and recovery. Benchmark realistic result sizes and fan-out before choosing more complex execution machinery.

## 16. Open decisions for the next design review

| Decision | Current recommendation | Alternatives / evidence needed |
| --- | --- | --- |
| Authentication and access-policy ownership | Confirmed: bring an existing auth provider/library; public functions accept anonymous callers with null identity; ordinary function code/shared helpers explicitly require sign-in and permissions; optional `.extend` adds computed properties directly to `ctx` and validated properties to the second handler argument (`input`), with callback shorthand; duplicate keys and reserved context names are rejected | Row/table-level enforcement is optional and advanced; detailed auth adapter contracts remain open |
| Public API syntax | Confirmed 12 October 2026: `query({ input?, output?, handler, ...options })` and `mutation(...)`, with callback-only shorthand and `ctx` first, validated input second; output inferred from the complete handler return and runtime-validated for supported serializable values, with optional explicit output schema checks | Retry option names and numerical limits remain open; examples are illustrative |
| Rust authoring and calls | Agreed: table/input derives, query/mutation attributes, generated direct async imports and query watch namespaces such as pending::watch() | Derives, typed contexts/selectors, registration and scoped calls are implemented with an embedded runtime; framework request scopes and native admin transactions are implemented; client-only bindings and a generic HTTP/SSE runtime with SQLite recovery are implemented |
| Function imports and connection | Confirmed: browser bindings call mounted query/mutation entry points; server code uses generated direct database access and `db.transaction(callback)`; reuse ordinary helpers with `ReadDatabase`, `WriteDatabase`, or full `Database` capabilities; no local `.run`, server `withAuth`, `createClient`, or client tree | Connection/bootstrap and same-logical-import build transforms remain open; helpers inherit caller scope and restrictions |
| Optional API/DB colocation | Reexport the same DB callables under the API entry module if one namespace is desired; no separate shared-client SDK | API contract adapter syntax remains open; retain response contracts, session lifecycle and DB-owned state/recovery |
| Primary application read API | Confirmed 12 October 2026: first release uses named reactive server functions with directly imported generated callables | Local row querying is outside the release; superseded implementations are removed during consolidation |
| Fullstack/serverless integration | One source repository, server-only direct database API and separately mounted generated query/mutation transport; application routes/framework functions own external work | Application-hosted transaction bridge and fully serverless subscription coordination require separate guarantees |
| Schema authoring | Rust or TypeScript native definitions compiled to one canonical representation; one source per project | Single-language frontend or neutral source file; validate authoring ergonomics and semantic parity |
| Initial query expressiveness | Named server functions using constrained database queries with projections and bounded results | Broader relational DSL; assess joins and pagination costs |
| Write presentation default | Server-confirmed, with explicit optimism | Application-wide optimistic default where predictors exist; typed optimism API remains open |
| Cached result presentation and persistence | Confirmed: wait for the server on cold reads; share current live results; preserve authorized open views as stale during reconnect; retain previously observed queries live for five idle minutes, configurable and capacity-bounded, then unsubscribe and evict | Revalidate after subscription loss; exact capacity limits, disk storage and optional stale-result display remain separate design choices |
| Application concurrency policies | Confirmed 12 October 2026: restricted declared mutations use serializable transactions with bounded automatic OCC replay; ordinary server transaction callbacks run once by default; both preserve managed integrity and invalidation | Retry budgets remain open; current generic runtime keeps serializable mode opt-in; no partial replay inside a run-once outer transaction |
| Managed writes and synchronization | Confirmed: atomic data/policy revisions with durable receipts and authorized result delivery; retain shared recovery/storage components used by this path | External-writer connectors need a separate future design; no competing live sources |
| SQLite release timing | Early contract-validation spike; production support later | Ship both only if resources and conformance evidence justify it |
| Tenant isolation | Separate application namespaces and enforced auth; choose physical deployment after workload tests | Database/schema per application vs. pooled storage, with operational and cost implications |
| License | Free self-hosting is required; exact license unresolved | Review ecosystem goals and competing-hosting policy |
| Managed cloud | AWS provisionally | Validate alternative providers with workload and cost estimates |

## 17. Reference material

Reviewed during the architecture discussion on 29 September 2026. These are design references, not adopted dependencies.

- Zero: [queries](https://zero.rocicorp.dev/docs/queries), [mutators](https://zero.rocicorp.dev/docs/mutators), [authentication and permissions](https://zero.rocicorp.dev/docs/auth), [ZQL](https://zero.rocicorp.dev/docs/zql).
- Convex: [architecture overview](https://docs.convex.dev/understanding/overview), [realtime](https://docs.convex.dev/realtime), [optimistic updates](https://docs.convex.dev/client/react/optimistic-updates).
- PowerSync: [service architecture](https://docs.powersync.com/architecture/powersync-service), [client architecture](https://docs.powersync.com/architecture/client-architecture), [consistency](https://docs.powersync.com/architecture/consistency), [sync streams](https://docs.powersync.com/sync/streams/overview).
- Electric: [sync overview](https://electric.ax/docs/sync/), [shapes](https://electric.ax/docs/sync/guides/shapes), [authorization](https://electric.ax/docs/sync/guides/auth), [writes](https://electric.ax/docs/sync/guides/writes), [Electric Circuits repository](https://github.com/electric-sql/electric-circuits).
- InstantDB: [architecture](https://www.instantdb.com/essays/architecture), [permissions](https://www.instantdb.com/docs/permissions).
- LiveStore: [sync architecture](https://docs.livestore.dev/building-with-livestore/syncing/), [Store API](https://docs.livestore.dev/api/livestore/classes/store/).
- PostgreSQL: [logical decoding and snapshot concepts](https://www.postgresql.org/docs/current/logicaldecoding-explanation.html), [transaction isolation](https://www.postgresql.org/docs/18/transaction-iso.html), [explicit locking](https://www.postgresql.org/docs/18/explicit-locking.html).
- SQLite: [write-ahead logging and concurrency](https://www.sqlite.org/wal.html).
- Turso: [engine repository](https://github.com/tursodatabase/turso), [SQLite compatibility](https://github.com/tursodatabase/turso/blob/main/COMPAT.md).
- Hosting: [AWS Fargate](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/AWS_Fargate.html), [RDS Multi-AZ](https://aws.amazon.com/rds/features/multi-az/), [Fly volumes](https://docs.fly.io/volumes/overview/).
