# Agreed SDK architecture

Status: target contract, consolidated 16 October 2026. This describes the replacement
API, not features already available in the current implementation.

Read this document before implementing a work packet in the
[delivery plan](sdk-implementation-plan.md). It establishes the architecture and
latest decisions. [backend-functions.md](backend-functions.md) retains detailed
edge-case decisions and an inventory of the current foundation; historical examples
there do not override this contract. If an uncovered detail affects observable
behavior, record the proposal and resolve it before shipping that behavior.

## Responsibility and execution boundaries

`@hulla/db` provides database access, database transactions and reactive server
queries. Application frameworks and servers own external services, jobs, endpoint
orchestration and authentication flows.

| Surface | Execution | Data access | Commit and replay |
| --- | --- | --- | --- |
| `query` | Server; browser calls its generated binding | `ReadDatabase`, one consistent snapshot | No writes; tracked for live invalidation |
| `mutation` | Server; browser calls its generated binding | `WriteDatabase`, one transaction | Atomic commit; bounded OCC retries after confirmed rollback |
| Direct server operations | Application server code | Generated `Database` | Operations commit independently unless grouped |
| `db.transaction(callback, options?)` | Application server code | Scoped `WriteDatabase` passed as `tx` | One commit/rollback; callback runs once unless explicitly opted into retries |

`WriteDatabase` extends `ReadDatabase`. `Database` is the full server capability,
including transaction management, not a union type. Shared ordinary functions take
the least capable database handle they require. Queries, mutations and server code
reuse these helpers rather than invoking declarations through `db.run` or `tx.run`.
The root database handle cannot execute inside its transaction callback; using it
there fails before execution. This guard must follow the async scope without
blocking unrelated concurrent requests. Closed handles reject further execution.

Declared queries and mutations must enforce their restrictions across imported
helpers, not merely hide APIs in TypeScript. External network effects and filesystem
writes are unavailable; time, randomness and shared mutable state must not undermine
safe replay or tracked reads. The [restricted native runtime](typescript-functions.md)
now enforces this boundary; package compatibility and configured limits remain
implementation constraints.

Decision confirmed 17 October 2026: v1 declared queries/mutations run in a restricted
JavaScript environment. Support ordinary computation, database capabilities and
compatible bundled JavaScript dependencies, including validation and transforms.
Do not promise arbitrary npm compatibility. Network/filesystem access, framework
server APIs, Node/Bun-specific libraries and native dependencies belong in ordinary
application server code. Imported helpers and validators have the same restrictions
as the handler; async validation does not grant external I/O access. Detect unsupported
imports during generation where possible and enforce capabilities at runtime.
Shared helpers remain reusable when their dependencies fit this environment.
Test and document representative validation/utility packages before claiming support;
Zod proof coverage alone is not a universal compatibility guarantee. The engine is
an implementation detail, and direct application server code keeps its host runtime.

Separate mutations are dispatched concurrently. Sequential `await`s can express
dependencies; already-started handles are still concurrent. There is no SDK queue
or inferred per-table serialization. OCC guarantees serializable database behavior,
not submission order or latest-click-wins semantics.

Declared mutations default to three conflict retries after the first attempt, with
short randomized backoff and a shared execution deadline. Direct transactions use
`db.transaction(callback, { retries: 3 })` for explicit opt-in; omission means zero.
Global settings cannot silently enable replay for arbitrary server callbacks.
Only eligible conflicts with confirmed rollback may replay the whole callback.
Uncertain commits and arbitrary application errors never trigger this replay.
External effects in application code belong after a retried transaction completes.

## Deployment contract

Decision clarified 19 October 2026: support reasonable cloud environments through
one provider-neutral deployment model. Application hosts may run frontend, SSR,
API routes and framework server actions using generated direct `db` operations and
`db.transaction`. Browser callables connect to a persistent `@hulla/db` service,
which executes declared functions and maintains subscriptions. Both server paths
connect to the same PostgreSQL database, self-hosted or managed.

Direct database access and the service require Node/Bun with native subprocesses,
matching executable assets and PostgreSQL connectivity. Containers, virtual machines
and suitable function hosts use the same SDK. Next/Vercel is one compatibility
fixture, not an exclusive target. Runtimes without native process support cannot
host these server components; their browser applications can still use the service.

Keep the same authoring API, transaction guarantees, auth boundaries and live
invalidation across deployment layouts. Do not require a managed hosting product,
provider selectors or a second database implementation. Credentials remain server-only.
Deploy the persistent service separately from short-lived application invocations.

Hosted support requires evidence for native asset loading, concurrent requests,
connection/process lifecycle, interruption and live updates on the selected host
and actual PostgreSQL endpoint. Local package/Node/Bun/Next tests are necessary but
do not establish deployed compatibility or pooler support. See [deployment](deployment.md)
for current packaging and lifecycle behavior.

## Authoring, generation and configuration

- Separate `defineSchema` and `defineConfig`. Schema object keys are database table
  names. No table discovery or separate physical-name override is required.
- Decision confirmed 17 October 2026: use `./db/schema.ts` by convention, with an
  optional `schema` path override in `defineConfig`. Discover exported query/mutation
  declarations recursively beside the resolved schema file by default. An optional
  `functions` directory override changes that scope; no functions entrypoint or
  registry is required. Recognize actual declarations, including `.extend`-derived
  declarations, through static analysis rather than filename/export-name guesses
  or importing every project module. Exclude schema/config files, generated output,
  tests and dependencies from discovery roots. Ordinary imported helpers still
  participate in dependency analysis and bundling but are not endpoints. There is
  no `api.ts` registry or internal/exposure marking. Importing modules or running
  generation does not start a network listener.
- Decision confirmed 18 October 2026: running the supplied Hulla service serves
  the discovered query/mutation declarations automatically. Declaring one already
  expresses intent to make it remotely callable; no additional `browser` or `api`
  enablement switch is required. Server-only applications can simply omit those
  declarations and use direct database operations and ordinary helpers. Neither
  helpers nor direct database access become public function endpoints. Do not
  require handwritten routes, transport decoding, receipt handling or runtime
  factories in starters.
- Generate `_generated/functions` for schema-typed declaration constructors,
  `_generated/client/...` for browser-safe callables, `_generated/server` for direct
  database access, `_generated/tables` for query field references, and
  `_generated/types` for reusable types. Never include server implementations or
  credentials in browser output. Table definitions and generated table references
  are separate objects; reserve `$meta` on generated references for metadata.
- Decision confirmed 18 October 2026: retain explicit generated client imports.
  Authors maintain one declaration; generation emits small remote-call bindings
  and types, not duplicate handler implementations or a manual re-export registry.
  Browser bundles contain the callable identity and shared SDK transport, excluding
  the original handler and its server dependencies. Direct source imports requiring
  a bundler transformation are not a v1 requirement.
- Remote invocation is a typed RPC interface supplied by Hulla. The SDK/service
  owns transport, verification, subscriptions and confirmation; applications do not
  author per-function HTTP routes or select transports for ordinary calls. HTTP
  requests and SSE remain the implementation direction; RPC does not require a
  WebSocket rewrite. No browser service worker is required for online-only v1.
- Service startup and client connection setup must work without a framework or
  `@hulla/api`. The implemented development path uses `hulla-db serve`, config
  `service` address/origin settings, a provider-owned `auth(headers)` verifier and
  public `database.client` settings emitted into lazy browser bindings. See [the executable setup](browser-service.md).
  No `createClient` or additional function-exposure flag is required.

Application imports expose the supported errors, result/handle types and inference
helpers. Keep schema and configuration authoring on their separate `/schema` and
`/config` entrypoints instead of consolidating them at the root. Generated
transport, binding, declaration and worker setup use internal
entrypoints; application code does not construct those pieces. The superseded
server request adapter is removed. Query snapshots expose status/data/error and
stale/optimistic indicators, without transport connectivity or prediction IDs.

Declarations use `query({ input?, output?, handler, ...options })` and the equivalent
`mutation(...)`. `query(handler)` and `mutation(handler)` are callback shorthands.
Handlers take `(ctx, input)` in that order. Constructors are generated to bind the
schema without application-level factories or global type registration.

Use `v` for database declarations and Standard Schema for function input/output.
Input has no implicit coercion. Explicit validator transformations are supported.
Output validation/transformation runs before mutation commit, determines the public
output type and cannot transform thrown errors.

`.extend({ input?, handler })` and `.extend(handler)` return derived declarers.
Extensions run sequentially in declaration order, inside the same snapshot or
transaction. Each input schema validates the original submitted object immediately
before its stage executes. Disjoint parsed properties accumulate into `input`;
computed handler properties accumulate directly on `ctx`. Reject duplicate keys and
reserved context names statically where possible and at runtime. There is no
`locals` object or automatic Standard Schema field routing. Strict validators that
reject unrelated fields cannot transparently share a flat input payload; document
that limitation. Limits belong on final declarations, not on extensions.

One public error class is `DatabaseError({ code, message, details? })`. Intentional
instances expose their declared information; unexpected errors are sanitized.
Driver details, stack traces and credentials do not become public error fields.
Do not add `.public`, separate application-error classes or error-transform hooks.
Error codes must distinguish confirmed execution failure from confirmation failure.

## Authentication and authorization

Authentication is bring-your-own. The mounted function endpoint verifies credentials
and supplies typed `ctx.identity | null`. Anonymous invocation is allowed; invalid or
expired credentials produce an authentication error rather than anonymous fallback.
Handlers and extensions enforce authorization in ordinary code. Permission reads
participate in the same snapshot/transaction and live dependency tracking.

The browser calls `setAuth` with `{ key, expiresAt? }` or `null` from its existing
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

Cache identity includes query, validated invocation identity and auth scope. Fresh
page loads wait for server results. Live results are shared; idle subscription grace
defaults to five minutes with bounded retention. After subscription loss, reactivation
requires validation. No browser SQLite, persisted results, IndexedDB recovery,
offline mutation queue or cross-reload mutation recovery in v1.

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

`.withOptimistic(...targetDescriptors)` is the only optimistic wrapper. Each target
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

After interrupted confirmation, a mutation enters recovering and automatically looks
up its original authoritative receipt. It never resubmits the payload, replaces the
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

## Limits, integrations and release boundaries

Input and output defaults are independently 16 MiB of uncompressed encoded bytes.
Input is checked before validation/execution; output after transformation and before
commit. `defineConfig` supplies defaults, final declarations can override, and
extensions cannot. Server execution time excludes network transfer. Numeric read,
execution and operation budgets need measured defaults; do not confuse earlier
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
legacy API aliases or a parallel implementation. No pre-publication schema/protocol
version layers or upgrade migrations. Recreate incompatible development fixtures
explicitly; never silently reset data. Runtime revisions, source identities and row
concurrency versions remain correctness mechanisms.
