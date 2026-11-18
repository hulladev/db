# Agreed SDK architecture

Updated 6 November 2026. This is the current API/correctness contract. The
[architecture](architecture.md) records package and engine boundaries; the
[delivery plan](sdk-implementation-plan.md) identifies unfinished work. The
[schema/SQLite guide](sqlite.md) provides current examples. Detailed schema/query
rules remain in [backend-functions.md](backend-functions.md), but its older service
setup does not override this contract.

## Responsibility and execution boundaries

`@hulla/db` provides authoring, typed database access, transactions and reactive
results. Applications own routing, external services, jobs and authentication flows.
Rust and TypeScript can run on clients and servers. PostgreSQL and SQLite are
explicit storage choices, with different current capability coverage.

| Surface | Execution | Database access | Commit and replay |
| --- | --- | --- | --- |
| Local declared query | Application instance | Readonly scoped `ctx.db` | No writes; local subscriptions track instance changes |
| Local declared mutation | Application instance | Writable scoped `ctx.db` | One local transaction; no inferred server handler replay |
| Remote declared query/mutation | Managed service | Read snapshot / managed transaction | Server authorization; bounded PostgreSQL OCC retry after confirmed rollback |
| Direct database operations | Application runtime supported by the adapter | Instance database API | Independent operations unless grouped |
| `db.transaction(callback, options?)` | Application runtime | Writable `tx` | One commit/rollback; callback runs once unless retries explicitly enabled |

`WriteDatabase` extends `ReadDatabase`; `Database` includes transaction management.
Helpers take the least capability they require. They retain their caller's snapshot,
transaction and restrictions. Root execution inside its own transaction callback
must reject before executing; completed transaction handles reject further use.
Scope guards must not block unrelated requests. Queries cannot obtain writes through
a type cast or captured root connection. Applications initialize once and share the
instance rather than creating/closing connections per query.

Managed remote TypeScript declarations run in the restricted native worker. Ordinary
computation and compatible bundled JavaScript dependencies are supported; arbitrary
Node/Bun/native modules, network calls and filesystem effects are not. Restrictions
cover imported helpers and validators. Local application handlers run in the host
runtime, not this sandbox; they currently receive `identity: null`. Do not claim
local handlers have remote worker isolation or automatic PostgreSQL retry behavior.

Restricted PostgreSQL mutations default to three conflict retries after the first
attempt, with bounded backoff and a shared deadline. Direct transactions default to
zero retries. Only eligible conflicts with confirmed rollback replay a callback;
uncertain commits and business/application errors do not. External effects belong
outside retried transactions. SQLite's transaction implementation and serialization
must preserve its documented guarantees without pretending to implement PostgreSQL
OCC internally. Separate remote mutations are concurrent, without inferred
per-table ordering; the opt-in offline queue has its own FIFO submission contract.

## Declarations, instances and registration

Import `defineSchema`, `defineTable` and validators from `@hulla/db`. Schema keys
name tables. Schema imports are inert and reusable across independent application
instances. Standalone table `.extend()`, `.pick()` and `.omit()` create independent
immutable definitions; bound table projections retain source ownership and keys.
Explicit `.replica()` declares readonly replica tables.

`schema.query(...)` and `schema.mutation(...)` construct function declarations.
Authors may destructure these helpers from the schema; no generated declarer import,
`defineFunctions` or separate `defineDatabase` is needed. Functions do not register
by being declared. An instance selects one barrel import loader or build-time glob;
a barrel combines shared/app exports using normal module composition. Catalogue
ownership rejects foreign-schema declarations before driver opening.

`sqlite({ schema, filename/open, functions?, sources? })` and
`postgres({ schema, url, namespace?, functions? })` create eager application instances
from the optional adapters. Operations wait for startup; `ready()` is an optional
explicit wait. Observable state reports initializing/ready/error/closed, independently
of network connectivity. `close()` is optional terminal shutdown. No public connect
step or startup mode option is required.

Ordinary opening verifies existing storage. `.initialize(...)` explicitly provisions
it; neither silently resets incompatible data or applies migrations. SQLite schema
migrations remain work. The existing PostgreSQL additive migration workflow retains
its explicit contract.

Local calls use inferred `db.functions` from a barrel, or `db.bind(declaration)` and
`db.bind(namespace)`. Binding does not register handlers. Local declarations must
be in the instance's selected catalogue. Shared handlers use invocation `ctx.db`,
not a captured instance. Applications with different schemas share ordinary helpers
or factories producing appropriately owned declarations.

The [optional glob build integration](build.md) rewrites literal relative `functions`
glob properties into module imports. Managed generation also compiles glob selectors.
Direct browser apps perform no filesystem scanning. No generated database wrapper,
second runtime or injected shutdown hook is necessary for this feature.

## Generation and remote sources

Generation statically inspects an application entrypoint such as `db/server.ts`
and emits `_generated/server` beside it. It imports the separate inert exported
schema and selected declarations without executing the eager initializer.
Generated modules allow both granular imports and namespace calls. Remote references
are unbound until `db.bind(...)`; browser output has contracts and type-only handler
imports, excluding server implementations, credentials and driver openers.

Generated `source` contracts carry schema/source ownership, not deployment URLs.
A device instance supplies typed `sources: [{ database: source, url, ...auth }]`.
Every replica owner must be provided; missing, extra, duplicate and mismatched
sources reject before storage opens. Multiple upstream databases are supported.
Storage connection URLs and service endpoints remain separate configuration.

Server queries declare their replica projection and authorize row membership through
the same query handler/context API. They return a real source selection; arbitrary
mapped or forged arrays cannot establish replica coverage. `.pick()`/`.omit()`
retain the primary key. Replica tables reject direct local writes; local-only tables
have no implicit upstream relationship. Devices read the union of downloaded
coverage, not an assumed whole server table.

Managed serving owns RPC routes, validation, identity verification, transactions,
subscriptions and receipts. Registration on a local instance does not mount an HTTP
service. Clients need no per-function forwarding routes or separate runtime factory.
HTTP/SSE remains the transport; browser service workers are not required.

## Deployment and tooling boundaries

PostgreSQL managed hosting is implemented with native executables and Node/Bun
subprocess support. SQLite local application storage is implemented, but SQLite
managed HTTP hosting and the native Rust SQLite database adapter remain unfinished.
Do not apply the PostgreSQL host/process requirements to every SQLite application.
Production topology and runtime claims require adapter-specific validation.

Applications can use direct storage without remote declarations. Remote applications
need a persistent service for subscriptions; short-lived framework invocations need
not own that service. Authentication/business authorization stay at the appropriate
application and managed handler boundaries. Server credentials never enter browser
output.

DB CLI/codegen is kept out of runtime import graphs. The current executable is
`hulla-db`; delegation from the existing `hulla` CLI to project-installed SDK tooling
is a recommendation under review. PostgreSQL dev accepts initializer entrypoints:
omitted URLs use managed local storage, while explicit URLs use external storage
without fallback or automatic provisioning. Generation prepares server-only settings
that read environment values at hosting time, without opening an eager instance.
Native asset extraction and managed SQLite hosting remain incomplete. CLI JavaScript inclusion does not require
bundling compilers or PostgreSQL servers for every consumer. See
[package and development decisions](architecture.md#packages-and-development-tooling).

The older `defineConfig` and generated `/server` setup still supports the existing
PostgreSQL service/deployment workflow. Its guides describe that implemented path,
not the new engine-neutral authoring direction. Operational settings still consumed
there must move with a reviewed replacement, rather than disappearing from docs
before the implementation exists.

## Function contracts and errors

Declarations use `query({ input?, output?, handler, ...options })` and the equivalent
`mutation(...)`, with callback shorthands. Handlers take `(ctx, input)`. Use `v` for
database fields and Standard Schema for function inputs/outputs. No implicit coercion
is added. Explicit transformations run before commit and determine the public output.

`.extend({ input?, handler })` and `.extend(handler)` derive declarers. Stages run
sequentially in the same snapshot/transaction. Each input schema validates the
original submitted object; disjoint parsed properties accumulate into input and
computed properties onto ctx. Duplicate/reserved keys reject. Strict schemas cannot
transparently share unrelated flat input properties. Limits belong on final
functions, not extension stages.

`DatabaseError({ code, message, details? })` exposes intentional application errors;
unexpected errors are sanitized. Driver details, credentials and stack traces remain
private. Confirmed failure and confirmation uncertainty have distinct codes.

## Authentication and authorization

Authentication is bring-your-own. The mounted function endpoint verifies credentials
and supplies typed `ctx.identity | null`. Anonymous invocation is allowed; invalid or
expired credentials produce an authentication error rather than anonymous fallback.
Handlers and extensions enforce authorization in ordinary code. Permission reads
participate in the same snapshot/transaction and live dependency tracking.

For source-bound instances, use `db.setAuth(source, valueOrPromise)`. The existing
remote runtime exposes the same auth lifecycle through its own `setAuth`. The caller
supplies `{ key, expiresAt? }` or `null` from its existing
auth subscription, or a promise resolving to either. The payload implies state;
there is no caller-supplied status field. Credential transport is configured
separately, with no `getToken` inside `setAuth`. Browser metadata never establishes
server identity. Account, session or tenant changes retire old cache/subscriptions and fence
late responses. Server-side expiry and access revocation must also stop live-query
delivery; client listeners alone are insufficient. Mutation confirmation follows
the original-response delivery policy below.

An unresolved auth promise immediately retires the previous scope and blocks
requests with `authentication_pending`. Awaiting `setAuth(promise)` waits for the
update; only the latest update may install state. Superseded resolutions/rejections
and resolutions after disposal are ignored. A current rejection or invalid resolved
value rejects that completion promise and blocks requests with
`authentication_failed` until a subsequent update. Failure never falls back to
anonymous access. Renewing through a promise always retires the previous scope;
synchronous same-identity updates preserve a still-valid cache.

Clarification updated 19 October 2026: the auth system owns session validity,
refresh and revocation. An authenticated `setAuth` object may optionally include
`expiresAt`, a non-negative integer Unix timestamp in milliseconds. Without it,
the existing application/provider subscription calls `setAuth(null)` on expiry or
sign-out. With it, the client also clears auth-scoped state at the supplied deadline;
a past deadline clears it immediately. This is local invalidation metadata, never
server authentication evidence or a required verifier envelope.

Changing only the deadline preserves a still-valid cache; omitting it on a subsequent
update removes the deadline. Renewing after expiry cannot revive retired handles.
Delayed browser timers are backed by deadline checks before exposing results or
sending requests. The client neither refreshes credentials nor subscribes to a
particular provider itself. Credential transport remains separate. Subsequent calls
are still subject to server verification; local expiry does not revoke a credential.

Server credential checks defer to the trusted verifier. An unchanged identity key
is not evidence that credentials remain valid. Live delivery must use verifier
revalidation or trusted provider invalidation signals rather than treating the first
verification as valid forever. The managed host revalidates original credentials;
see the implementation plan for coverage and remaining integration work.

Direct server database access has application-owned authentication and authorization.
It needs no identity binding, `withAuth`, request discovery or framework/provider
combination matrix. Better Auth is an optional self-hosted reference integration,
including storage/schema support; it must not become a core dependency. Other auth
providers can use the same trusted verifier boundary. Advanced policies are optional,
not a prerequisite for CRUD or a second permissions language in the quickstart.

## Schema and database operations

The detailed [schema decisions](backend-functions.md#target-schema-validation-and-inferred-types)
and [data representation decisions](backend-functions.md#target-single-row-reads-and-aggregation)
remain part of the accepted scope. Key invariants:

- Schema inference includes `$inferSelect`, `$inferInsert` and `$inferUpdate`.
  Optional adapters such as `@hulla/db/zod` expose `createSelectSchema`,
  `createInsertSchema` and `createUpdateSchema`. Preserve convertible constraints;
  do not pretend uniqueness, foreign keys or arbitrary SQL checks are local validators.
  Generated schema aliases record resolved table-reference identities. ID types
  accept plain validated scalars while rejecting known IDs from another table.
- Database constraints include supported value bounds, uniqueness, references and
  advanced named SQL checks. `v.id(tableDefinition)` and `v.id("tableName")` identify
  real foreign keys; strings avoid circular imports. Deletion defaults to restrict,
  with explicit cascade/set-null policies. No relationships/include DSL in v1.
  Scalar `.min`/`.max` bounds and `.oneOf` choices apply to columns; constrained
  JSON members explicitly reject until the native JSON contract can preserve
  their rules. Put key-domain constraints/codecs on the target primary key;
  reference-specific subset rules use named SQL checks.
- Explicit schema indexes; no query-driven automatic index creation. Query plans
  select indexes. Unindexed queries remain possible within execution limits.
- Built-in representation modes and synchronous deterministic bidirectional codecs
  map application values to storage. Comparisons and ordering retain storage semantics.
  No silent unsafe-integer rounding, arbitrary class transport or inferred coercion.
- Timestamp defaults are timezone-aware Date with millisecond precision. Calendar
  dates default to strings. Explicit date/string/timezone/precision choices follow
  the recorded restrictions; never silently use machine-local timezone or truncate
  finer precision into Date.
- `v.json()`, type-only `v.json<T>()`, and validated JSON are supported. Document
  `v.json({ ... })` as shorthand for a validated object inside JSON. `v.object` and
  `v.array` describe JSON shapes. Conversions inside JSON must be explicit.
- Updates replace supplied JSON columns as a whole. Top-level omitted/undefined
  update fields remain unchanged; null is explicit and requires nullability.
  Reject undeclared properties. No nested JSON patch API in v1.

Database builders are immutable and lazy: consumption starts execution; repeated
awaits of the same object reuse its outcome. New branches are separate operations.
Reject modifiers after execution starts. This differs intentionally from browser
operation handles, which start on invocation.

| Operation | Accepted contract |
| --- | --- |
| `select(table, projection?)` | Rows by default; array and object projections |
| `.where(field, operator, value)` | Explicit operator; string paths or generated references |
| `.where({ ... })` | Equality predicates |
| `.where(({ and, or, eq, ... }) => ...)` | Explicit expression tree for compound predicates; repeated where calls AND whole predicates |
| `.limit(n)` | Always an array, even for one; terminal and exclusive with paginate |
| `.unique()` | Row or null; error for multiple rows |
| `.aggregate(a => ({ ... }))` | Named count/sum/min/max/avg results; SQL empty/null semantics; groupBy/having supported |
| `insert(table, data)` | Single row or bulk rows, with IDs; `.returning` narrows by array/object projection |
| `update(table, patchOrExpressionCallback)` / `delete(table)` | Explicit where required, including explicit true for all rows; return affected rows, narrowed with array/object `.returning` |
| `.expectRows(n)` / `.expectRows(count => ...)` | Synchronous cardinality guard inside the transaction; mismatch prevents commit |
| `.onConflict(({ on }) => on(...fields).update(...))` | One actual primary/unique key; atomic update or `.skip()`; no separate upsert |

Conflict updates accept a patch or `({ current, incoming }) => patch` of SQL
expressions. Constraints remain enforced. Skip returns null for a single insert and
omits skipped entries from bulk results. Bulk returning order is unspecified.

String paths are table-relative, never optionally table-prefixed. Generated field
references support column comparisons. Equality/inequality with null normalize to
IS NULL/IS NOT NULL; explicit null operators remain. Undefined filter values error.
Text matching defaults to case-sensitive, with explicit override; literal substring
helpers differ from SQL wildcard `like`. JSON presence and null are distinct:
exists includes an explicitly null property, while null checks cover missing/null
values as recorded. Keep the full truth tables and array/codec behavior in the
[filter contract](backend-functions.md#target-filtering-expressions).

## Browser results and reactive cache

Calls return immediately-started awaitable handles. Await resolves once to the
authoritative result; repeated awaits do not execute again. Subscriptions receive
full snapshots. Direct `query.subscribe(input?, listener, options?)` is a convenience.
An await-only query is one read; observing keeps it live. No `.result` or `.watch` API.
Mutations report one invocation, not a continuing query.

Query statuses are pending/success/error; mutations additionally use recovering.
Data/errors and stale/optimistic indicators follow the recorded snapshot contract.
Query results are recursively readonly ordinary objects/arrays, including awaited
results. Updates require copies; development mutation checks must not promise that
freezing properties makes Date internals immutable.

Cache identity includes query, validated invocation identity, source and auth scope.
Ordinary cold remote reads wait for the server. Live results are shared; idle
subscription grace defaults to five minutes with bounded retention. After
subscription loss, reactivation requires validation. Explicit SQLite replica storage
and offline mutations have durable persistence and scope fencing; these are opt-in
capabilities rather than a change to every remote query's cache policy. Replica
query reads cover downloaded authorized rows, including offline; missing coverage
is not evidence that no matching server rows exist.

Coordinate actively observed queries within one runtime, database source and auth
scope. Successful current results published together must describe one database
state. Revision labels alone do not establish this. Related pagination pages share
the guarantee; cross-client/tab/database coordination is outside scope. Mutation
await means server commit confirmed, not all subscriptions refreshed.

Infrastructure failure retains the previous authorized coherent view as stale.
Handler errors clear that query's data by default; global/per-subscription
`keepPreviousDataOnError` may preserve it as stale. Auth/access failure always clears
affected data. Relevant database changes can recover failed queries automatically,
even with timed handler retries set to zero. Timed query retries default to zero;
transport reconnect is independent. Query abort is supported; mutation cancellation
is not. One-shot query confirmation defaults to a configurable 30-second deadline.

## Live pagination

Server `.paginate({ limit, cursor })` returns a page and `cursor`. Limit/cursor can
be ordinary validated function inputs or server constants, never injected universal
client options. Order defaults to primary key; custom order gets a deterministic
primary-key tie-breaker. Cursors bind to query conditions and ordering. Live pages
have stable ranges; they can grow/shrink rather than continually refill fixed counts.
No grouped aggregate pagination in v1.

Eligible generated queries expose:

```ts
const tasks = listTasks.paginate(cursor => ({ projectId, limit: 20, cursor }));
const unsubscribe = tasks.subscribe(renderTasks);
await tasks.next();
```

`.next(pages = 1)` appends sequentially using returned cursors, stops at exhaustion,
and is then a no-op. `hasMore` reports continuation. Overlapping calls share the
in-flight load; the first determines its requested page count. Loading uses pending
with existing data retained, not separate loading-more flags. Append failure retains
successful pages; the next call resumes after the last success. Auth rules override
retention. Disposing releases related page subscriptions together.

Filters, sorting and other non-cursor inputs stay stable for a handle. Changing them
creates a fresh list and releases the old one. Framework adapters can manage this
lifecycle; arbitrary captured JavaScript variables are not automatically tracked.
Decision confirmed 17 October 2026: a live range exceeding its execution limits
produces an explicit limit error for the affected paginated list. Never silently
truncate, shift range boundaries or automatically split pages. Apply the existing
query error policy to the list as a whole: clear its data by default, or retain its
last coherent snapshot as stale when `keepPreviousDataOnError` is enabled globally
or for that subscription. Auth/access failures still override retention. Relevant
database changes automatically retry evaluation and restore the list when it fits
within limits; ordinary timed query retry defaults remain unchanged. Users may
narrow filters or configure higher limits. Automatic range splitting is outside v1.
This refresh-overflow policy does not change append-failure semantics above.

## Optimism and mutation confirmation

`.optimistic(...targetDescriptors)` is the optimistic wrapper. Each target
identifies a query and supplies a pure immutable cache updater. No-input queries
need no input mapper. Input-bearing queries require an exact input mapper or a
match predicate selecting existing cached variants; matching needs no mapper. Separate exports
provide base and optimistic callables. No per-call opt-out or predicted mutation output.
Missing cached results are skipped, not seeded or retained for later prediction.

Apply per-invocation layers in submission order over the latest server base. Remove
only the affected layer and replay others on failure. Successful layers remain until
query state incorporates the commit. Transport interruption removes the layer
immediately while confirmation continues; recovery never reapplies it. Updater
failure removes all that invocation's targets, leaves server execution alone, and
emits a minimal development diagnostic. No logging subsystem.

For ordinary online calls, interrupted confirmation enters recovering and automatically
looks up the original authoritative receipt. It never resubmits the payload, replaces the
request identity or requires a manual recovery call. Checks continue across connection
failures with bounded backoff within a configurable 30-second total deadline measured
from initial submission. Success/failure settles the original handle; expiry produces
a distinguishable confirmation error, not evidence of rollback or permission to retry.
Confirmation delivers the original authorized response to the same currently verified
caller, data scope and authentication scope. Database permission changes after commit
do not retroactively revoke that response. Do not rerun handlers, extensions or
validators, or require the original declaration to remain registered, to confirm it.
Source and expiry fences still apply; clients still decode results with their own
contract. Invalid credentials or changed identity/authentication scope block delivery.

No public unknown status. A missing receipt alone is never confirmed failure. A known
commit with an expired result remains distinguishable from an unconfirmed outcome.

Decision confirmed 17 October 2026: lookup delivers the original authorized response,
rather than performing a new database operation. It independently verifies current
credentials and binds delivery to the original caller, tenant and authentication
scope. Identity/tenant changes and invalid, expired or revoked credentials block
delivery. Database permission changes after commit do not retroactively revoke the
original response. Lookup never reruns handler/extension guards, reexecutes the
mutation or requires a second application authorization policy. This exception is
limited to short-lived confirmation of that invocation; live queries still reevaluate
current permissions. Browser cache keys are never proof of identity or scope.

Receipts are short-lived operational state derived from the accepted confirmation
window plus cleanup grace. ACK may remove full payload early; duplicate protection
must survive until safe expiry. Server-verifiable fixed request validity must prevent
old requests executing after markers are purged. Do not purge active executions or
shorten accepted protection after config changes. No 24-hour result archive or
30-day status archive. Applications own durable history and undo tables.

Decision confirmed 17 October 2026: reuse a short-lived server-issued admission
ticket obtained during connection setup, rather than requiring a preflight for every
mutation. The SDK renews tickets proactively for future calls; a new call may wait
for renewal when no valid ticket is available. Authentication and ordinary handler
authorization remain independent of the ticket. Bind tickets to source and verified
caller/tenant/authentication scope. Each invocation keeps its original ticket and
request identity throughout execution and confirmation; renewal never resubmits or
reidentifies an existing invocation. First admission fixes the function and input
integrity for that request identity, rejecting conflicting reuse.

Full response retention follows the accepted confirmation window. Compact duplicate
protection survives at least the ticket's execution validity and accepted invocation
protection, plus cleanup grace; active work remains protected. Ticket expiry prevents
new admission, not confirmation of an already accepted invocation within its window.
Implemented serving protocol: opaque random tickets
stored in the database, with permanent ticket retirement ordered before request
marker purge. No application signing keys are required. Accepted request records
remain independent of ticket existence, so expiry/retirement cannot erase a retained
outcome or interrupt active work. The current service uses five-minute tickets,
four-minute browser renewal, a 15-second execution-start window, 60-second result
retention and 30-second cleanup grace. Accepted windows are frozen per request.
These defaults still need loaded deployment measurement. Tickets are SDK internals,
not an application authoring API.

## Opt-in offline mutation behavior

A device-bound remote mutation opts in with `.offline(resolveConflict)`, optionally
alongside `.optimistic(...)`. This queues the server call without executing its
handler locally or permitting local writes to a replica. The device callback chooses
retry with revised input, rejection, or deferral to application UI; no default merge
policy or built-in conflict UI is supplied. Server business/concurrency guards and
authorization still apply. Register the resolver on a reopened instance before
recovering its queued calls.

Durable queues are FIFO per source/account. Unsent calls may be submitted when
connectivity returns; sent attempts use receipt lookup with their original identity
and ticket, never automatic POST resubmission. Unknown outcomes can block later
intents. Terminal evidence persists until explicit acknowledgement; cache eviction
must not remove it. Bounded capacity rejects excess work rather than silently
losing requests. Account/source changes fence recovery and protected rows.

The instance supplies `pendingMutations`, `resolveConflict`, `acknowledgeMutation`,
`flush`, source-specific `setAuth` and `forgetQuery`. Full examples and current
budgets are in [the SQLite guide](sqlite.md#explicit-offline-calls). Local readiness,
remote connectivity, stale results and optimistic feedback are separate concepts;
per-source observable connectivity is still work.

## Limits, integrations and release boundaries

Input and output defaults are independently 16 MiB of uncompressed encoded bytes.
Input is checked before validation/execution; output after transformation and before
commit. The existing managed service configuration supplies defaults; final
declarations can override and extensions cannot. Migration of those operational
settings to the new instance/hosting setup remains explicit work. Server execution
time excludes network transfer. Numeric read, execution and operation budgets need
measured defaults; do not confuse earlier
proposals with settled release values. Warn concisely near limits in development.

UI bindings ship separately as `@hulla/db-react`, `@hulla/db-vue`,
`@hulla/db-svelte` and `@hulla/db-solid`; the core has no UI framework dependencies.
They observe generated callables after mount, preserve equivalent encoded inputs,
and clean up with the component. Optional `initialData` supplies an observer-local
SSR fallback until the first live result; protected seeds carry `initialAuthKey`
matching the app-owned auth cache key. Inputs and identity changes permanently
retire seeds. Server loading uses direct `db`; hooks do not execute server reads.
Core `Input`/`Output` helpers infer submitted and resolved types for declarations
and both callable kinds, including optimistic wrappers.
Integration recipes target Astro,
Next.js, Nuxt, SvelteKit and SolidStart. Keep one framework-neutral runtime and cache,
with thin framework lifecycle bindings. Serverless consumers do not imply that a
long-lived database worker or subscription coordinator runs inside every serverless
request. Test deployment topology and SSR request isolation explicitly.

Remove superseded implementations, public exports, fixtures and guides as their
replacement lands. Preserve reusable internals and meaningful failure tests, not
aliases retained solely to archive an unreleased proposal or a parallel implementation.
Published setup paths still in use need an explicit migration before removal.
Do not invent compatibility/version layers for unreleased proposals. Application
schema upgrades follow the explicit migration workflow; incompatible development
fixtures may be recreated explicitly, never reset silently. Runtime revisions,
source identities and row concurrency versions remain correctness mechanisms.
