# Intended developer API

Planning entry point, 16 October 2026: read the consolidated
[agreed SDK architecture](sdk-architecture.md) and
[replacement delivery plan](sdk-implementation-plan.md) first. This document retains
detailed decisions and an inventory of the development foundation. Historical
implemented examples and infrastructure descriptions do not override the latest
accepted contract and are not the future application quickstart. No target API is
claimed implemented merely because it appears here.

**Status:** Agreed authoring direction, with an initial executable
[TypeScript function SDK](../packages/db/README.md). Callback declarations,
direct-import binding generation and runtime/dispatch boundaries are implemented.
The [schema compiler foundation](schema-authoring.md) now supports the table
declarations below. The [managed PostgreSQL function runtime](managed-functions.md)
now executes arbitrary canonical schemas with CRUD, registered Rust handlers,
atomic mutations, tracked reads and recovery receipts. Database execution examples
remain illustrative where they depend on the proposed builder syntax. The
[TypeScript transaction worker](typescript-functions.md) supplies schema-typed
contexts and executes authored callbacks through native managed transactions.
The [deployed adapters](sdk-adapters.md) now provide generic
HTTP/SSE serving, generated result validation, browser persistence, receipt recovery
and identity cleanup for registered native Rust handlers. The [Rust authoring API](rust-authoring.md)
now provides table/input derives, typed database contexts, query/mutation attributes,
registry adapters and task-scoped direct calls. Client-only Rust generation and
framework-specific authentication remain separate work.
[Production operations](production-readiness.md) now supplies framework request scopes,
typed native admin access, quotas, roles and deployment tooling.

## Function model at a glance

### Usage modes

Decision confirmed 12 October 2026: server-only applications use `@hulla/db` through
ordinary server-only `db.select`, `db.insert`, `db.update` and `db.delete` operations,
and `db.transaction(callback)` for grouped work. They need not declare a browser
operation for every database statement. Application endpoints and external-service
orchestration belong to the user's server code, optionally built with `@hulla/api`.

Applications using the package directly from a browser use declared queries and
mutations through generated direct callable bindings, with query subscriptions.
Discovery registers declarations with the executor; the running Hulla service
automatically serves them through its function API. No enablement switch is required.
Neither mode requires a DB `createClient`, a second
application API or handwritten forwarding routes. Both share the database engine,
constraints and committed-change invalidation machinery. Server initialization and
exact direct-operation signatures remain under design.

This is the target API, not a claim that every part is implemented. Declared handlers
execute on the server; browser bindings contain contracts and invocation stubs.

| Surface | Use it for | Database boundary | External services |
| --- | --- | --- | --- |
| `query` | Data clients read or subscribe to | Read-only `ctx.db` in one consistent snapshot | Unavailable |
| `mutation` | A complete client-requested database change | Read/write `ctx.db` in one automatic transaction | Unavailable |
| Server database operations | Ordinary backend database work | Managed individual reads/writes | External work remains ordinary backend code |
| `db.transaction(callback)` | Group related server database operations | One transaction; callback runs once by default | The SDK cannot make arbitrary server callbacks safe to replay |

There are only two browser operation declarations: `query` and `mutation`. External
work needs no database declaration or database-owned job runner. Ordinary reusable
functions need no declaration either.

### Calling and composing functions

Decision confirmed 12 October 2026: declarations are browser-facing query and
mutation entry points. Reuse ordinary functions rather than invoking declarations
from server code or other declarations. Keep handler logic inline by default and
extract a helper when there is actual reuse. The target API has no `db.run`,
`tx.run` or `ctx.run` method.

Helpers accept the least database capability they need:

| Capability | Available operations | Typical caller |
| --- | --- | --- |
| `ReadDatabase` | Schema-typed reads | Query `ctx.db`, or any broader database scope |
| `WriteDatabase` | Reads plus writes; extends `ReadDatabase` | Mutation `ctx.db` or transaction callback `tx` |
| `Database` | Full server database API, including transaction management | Ordinary application server code |

A query's `ctx.db` is read-only; a mutation's `ctx.db` and the `tx` passed to
`db.transaction` are writable. Passing a scoped handle to a helper keeps its work
in the caller's snapshot or transaction, including prior writes and tracked reads.
A helper must not capture a global database handle to escape the caller's scope,
restrictions, permissions or dependency tracking. Network calls and other external
effects remain outside declared handlers and their imported dependencies.

Root database execution inside its transaction callback must fail before executing
an operation, including CRUD, another root transaction or a helper capturing the
root handle. This guard is scoped to the async execution, not all concurrent
requests. Closed transaction handles reject further use. Scope propagation and
enforcement across supported runtimes require validation.

### Database builder execution

Execution timing confirmed 14 October 2026: database operations are awaitable
builders. Constructing and chaining a builder prepares the operation without
executing SQL; consuming its thenable through await, Promise assimilation or a
Promise combinator starts execution. Do not require `.execute()` or use timers,
chain-completion guesses, source inspection or ambient scope detection to decide
when a chain is finished. Apply this execution model consistently to reads,
writes and aggregates, including operations whose terminal methods remove further
builder modifiers. All attached predicates, conflict policies, row expectations
and returning projections must be present before execution starts.

An unconsumed builder is not an already-running native Promise. Returning it from
an async function or passing it to `Promise.resolve` consumes the thenable and
starts execution. Awaiting a statement on a transaction-scoped handle executes
within that transaction; it does not independently commit the enclosing
transaction. Preserve the distinct browser function contract: calling a generated
browser function starts its operation immediately and returns an observable,
awaitable operation handle.

Repeated consumption confirmed 14 October 2026: each database operation object
executes once and retains its execution Promise/outcome. The first consumption
starts execution; concurrent consumers join it and later awaits receive the same
result without executing another statement. Reuse a rejection as well as a success;
re-awaiting a failed object must not silently retry the write. Managed OCC retries
remain internal attempts of the enclosing logical mutation, not new executions
triggered by repeated awaits. Construct a fresh operation for an intentional new
execution.

Builder lifecycle confirmed 14 October 2026: chaining is immutable. Each modifier
creates a fresh builder without changing the source, so unexecuted bases may be
branched into independent operations. Each resulting operation object owns its
own execution Promise/outcome; structural query similarity does not deduplicate
separate database operations. Once consumption starts, reject further builder
modifiers on that operation, including attempts to attach returning projections
after a write has executed. Repeated awaits remain allowed and reuse its outcome.
Do not use this lifecycle rule to bypass transaction-scope or closed-handle guards.
Client query-result mutability is specified below; ownership of direct-server
operation results must not be inferred from client cache sharing.

### Commit, rollback and retries

Separate server writes or browser mutation invocations commit independently.
Grouping related server work in `db.transaction(callback)` gives it one commit or
rollback. The callback executes once by default: an arbitrary server callback may
contain code the SDK cannot safely replay. A serializable conflict is surfaced to
the caller, not silently retried. Unknown commit outcomes require recovery rather
than blind re-execution.

Direct transaction retries confirmed 16 October 2026: support
`db.transaction(callback, { retries: 3 })`, with retries counting additional
attempts after the initial attempt. Omission means zero retries. Require explicit
opt-in on each transaction call; global configuration must not silently enable
retries for ordinary server callbacks. Retry the entire callback only for eligible
concurrency conflicts after confirmed rollback, within the transaction's shared
execution deadline. Never retry an uncertain commit or an arbitrary application
error. Document that opting in makes the callback replayable: external effects
such as sending email belong after the transaction completes, not inside the
retried callback. A database rollback cannot undo external effects.

Restricted declared mutations retain bounded whole-handler OCC retries after
confirmed rollback. Their helpers inherit the caller's restrictions and retry
policy. Helpers do not introduce independent retries, commits or automatic nested
savepoints. An uncaught error rolls back the enclosing transaction. Catching an
ordinary helper error does not itself undo writes that already succeeded; the
caller must account for those writes before continuing. A database error that
invalidates the transaction cannot be made committable by catching it. A retryable
conflict rolls back the whole attempt before any whole-handler retry.

OCC retry budget confirmed 16 October 2026: declared mutations default to at most
three retries after the initial attempt (four attempts total). Use short randomized
backoff to reduce repeated collisions. All attempts and backoff share the existing
server execution deadline; retrying must not restart it. Configure the default
through defineConfig and allow overrides on final mutation declarations, not
extensions. Retry only eligible conflicts after confirmed rollback, never lost
responses or uncertain commit outcomes. Exhausting the conflict retry count
produces a distinguishable conflict DatabaseError; exhausting the execution
deadline remains an execution-limit failure. Validate these initial defaults
under contention before release. Exact option spelling and backoff intervals
remain implementation proposals. Direct db.transaction callbacks retain their
run-once default, with retry behavior enabled only by explicit opt-in.

Direct server writes and declared mutations participate in the same revision,
constraint and subscription invalidation machinery. Transaction changes become
visible atomically after commit; rolled-back changes publish nothing.

### Function discovery

Discover exported query and mutation declarations recursively beside the schema by
default, or in the configured functions directory, and register them with the executor.
Decision confirmed 18 October 2026: the running service serves those declarations
automatically; there is no additional `browser` or `api` enablement switch. Generation
supplies client bindings but does not start a listener. Server-only applications can
omit declarations. Ordinary helpers and direct server database operations are
not browser endpoints. There is no special `api.ts` or internal/exposure flag.
Function authors enforce business permissions and limit returned data; the service
enforces dispatch validation and authorization boundaries.

### Authentication boundary

Authenticated production use must support a fully self-hosted, open-source auth
integration without a mandatory paid service or provider account. Hosted auth
integrations remain optional. Better Auth is the intended optional reference
integration, including storage/schema support; applications may use other providers.

Decision confirmed 12 October 2026: bring an existing authentication provider or
library; Hulla does not implement accounts, login flows, passwords or its own
session system. Supported integrations should use established verification code,
with an extension point for a trusted server-side verifier. Application developers
should not need to implement cryptography to connect authentication.

The service's function endpoint verifies credentials and supplies
the application-typed `ctx.identity: Identity | null`. Ordinary server database
operations use application-owned authentication and authorization; identity is not
bound to `db`, and the target API has no server `db.withAuth`. Trust
comes from server verification, never from client-supplied user/tenant fields.
Hulla remains responsible for preserving the verification boundary, separating
user-specific results and enforcing access during live delivery. Applications
define business permissions. Automatic request discovery for each provider/framework
combination is abandoned; credential verification is configured at the mounted
endpoint. Exact adapter syntax remains to be settled.

Decision confirmed 12 October 2026: anonymous invocation is allowed by default.
Missing credentials produce `ctx.identity === null`; valid credentials produce the
verified identity. Invalid/expired credentials cause an authentication error, not an
anonymous fallback. Protected functions explicitly check identity in ordinary code;
there is no required anonymous-access declaration option.

Decision confirmed 12 October 2026: authorization is ordinary code in function
handlers, optionally factored into ordinary reusable helpers. Read the verified
identity, check access and select the permitted result or perform the permitted
operation. Database-dependent checks run inside the same snapshot/transaction as
the protected work. Hulla tracks reads made by permission helpers just like other
query dependencies, so membership changes can invalidate live results.

RLS and table-level rules are optional advanced capabilities, not getting-started
requirements. TypeScript authors do not need a separate Rust/native authorization
policy alongside their handlers. The runtime still enforces isolation, validation
and authorization-aware recovery/delivery; optional policies cannot substitute for
those guarantees. Optional identity/permission helpers and typed declaration
extensions should let applications reuse checks without a separate permissions
language. The agreed declaration API is `.extend({ input?, handler })`, returning a new
declarer; callback shorthand `.extend(guard)` is supported when no input is added.
Decision confirmed 12 October 2026: extension handler results add computed
properties directly to `ctx`; there is no `ctx.locals` wrapper in the target API.
Computed context is invocation-scoped and chained guards see earlier contributions.
Reject duplicate computed keys and reserved core context names such as `db` and
`identity`, statically when detectable and at runtime. Never overwrite core
capabilities or a prior extension's result.
Decision confirmed 13 October 2026: extension handlers execute sequentially in
declaration order, followed by the final function handler. Each extension sees
earlier computed context and the validated input accumulated through its stage.
A failure stops subsequent handlers and propagates through the enclosing
transaction's rollback policy. Decision confirmed 13 October 2026: validate each
stage immediately before executing its handler, accumulating its parsed input
after validation and duplicate-key checks succeed. After all extension stages,
validate the final function input and run the final handler. Successful output
validation/transformation must finish before a mutation commits. Stages execute
inside one invocation rather than requiring additional client/server round trips.
Measure validation and extension overhead before making latency claims; arbitrary
validator complexity and database work in guards can dominate simple field checks.
Extensions also support shared validated inputs and guards that use those inputs.
Decision confirmed 12 October 2026: each extension input validator and the function
input validator independently validate the original submitted object. Their parsed
properties combine into the handler's second argument. Use `input`, rather than
`args`, as the parameter name in documentation, matching the declaration property.
Reject duplicate parsed input keys even when their types match; separate validators
may apply different transformations or defaults. Reject statically known collisions
at type-check time and enforce duplicate rejection at runtime as well. This
supersedes placing validated extension inputs in `ctx.locals`.

The generated callable requires the combined input requirements of all validators;
the runtime does not merge their schemas. It combines disjoint parsed output
properties, never overwriting a key. A function's schema should therefore declare
its additional input fields rather than redeclaring inherited fields. Extending a
library schema does not mutate the original extension schema. Validators sharing
a flat input object must tolerate unrelated
properties; strict object schemas can reject another validator's fields. There is
no generic Standard Schema field introspection or automatic field routing.

Callback shorthand remains available when an extension adds no input schema. A
function using shorthand still requires its extensions' inputs and receives their
combined parsed properties in its second argument.
Extensions preserve the
base function kind, input/output validation,
snapshot/transaction boundary and retry restrictions. Database-dependent guards
run with the protected work and their reads are tracked. Guards remain ordinary
code: the API needs no inferred authentication requirements or auth type-effect
machinery.

Browser `setAuth` accepts `{ key, expiresAt? }`, `null`, or a promise resolving to
either, independently of UI framework or provider. State is inferred from the
payload rather than supplied through a status field. Pending promises retire the
previous scope and block requests until resolution; only the latest update applies.
Rejected resolution blocks access until the next update. Credential transport is configured
separately; `setAuth` has no `getToken` callback. Applications connect their existing
auth state listeners and own their lifecycle. A key change for a user, session or
tenant retires old watches/cache and fences late results from the old identity.
The key partitions client state; it is not proof of identity for the server.
An authenticated object may also provide optional `expiresAt` (Unix milliseconds)
to clear local state at a known expiry time. Omitting it preserves app-driven
invalidation; updating only the deadline preserves a still-valid cache. Server
credential verification remains independent.

## Authoring and client experience

Applications should declare their tables, write typed functions and use the
generated client API. Transport framing, receipts, revision fences, subscription
recovery, wire validators and route assembly are package implementation details.
They must not appear as copied code in an application starter.

Decision, confirmed on 12 October 2026: the first release uses reactive server
query functions with directly imported callable bindings and result watches. This
continues the 7 October authoring direction and supersedes the earlier initial-release
requirement for local row querying. Local row querying, read scopes, coverage and a
browser replica are outside the selected release. Superseded implementations and
their documentation are removed as the replacement lands, not maintained alongside
it. Git history preserves retired designs; shared components still used by the new
path remain. Exact API spelling remains provisional.

Query callbacks execute on the server; clients observe their returned results.
Cached results do not establish coverage for new filters or arguments. Optional
optimistic presentation can address mutation latency. The cache policy confirmed on
12 October waits for the server on cold reads, then shares and retains current live
results without repeated loading states. Retention across view changes is bounded;
immediate fresh reuse requires a maintained subscription. After subscription loss,
reactivation requires server validation. An already-open view can retain its last
result as stale during reconnect; account changes and access revocation retire
affected state. A prior-session persisted result is not displayed before server
validation by default. See [result freshness](architecture.md#result-identity-completeness-and-freshness).
Mutation delivery confirmed 16 October 2026: distinguish automatic
outcome reconciliation from resubmission. After interrupted confirmation, the
SDK automatically checks the original request's authoritative receipt under
appropriate authentication and authorization. It does not resend the mutation
payload or create a replacement invocation, even if no receipt is found. A
transport failure does not establish server failure, cancellation or rollback.
Receipt lookup must not execute mutation handlers or their declaration extensions.
Live-query reconnect remains automatic and independent. Confirmed
server-side OCC rollback retries remain part of transactional execution; these
are not client transport retries.
Cross-reload scope confirmed 16 October 2026: exclude mutation recovery across
page reloads from v1 entirely, including an opt-in persistence mode. This
supersedes the 15 October default IndexedDB recovery-persistence proposal:
do not persist or automatically restore mutation handles after reload.
Keep live query results, subscriptions and optimistic layers in memory; cold reads
after reload still wait for the server. Automatic outcome checks apply to
existing in-memory handles within their confirmation window. Reload does not
cancel or roll back a submitted server transaction. Applications needing durable
operation tracking own the relevant application tables and workflow. No browser
SQLite engine, IndexedDB recovery dependency, row replica, event log or offline
mutation queue is introduced. Missing receipts never authorize automatic new
mutation submissions. The bounded server receipt policy below remains necessary
for outcome reconciliation and duplicate protection; lookup-only reconciliation
does not eliminate per-request server storage. A terminal failure response must
guarantee the original invocation cannot subsequently commit; an absent receipt
alone is insufficient, including when the original delivery is delayed. The
application may deliberately submit a new invocation after a confirmed failure,
subject to ordinary validation/authorization. It must not infer failure from a
lookup timeout. The UI may offer a new submission after confirmed execution
failure. While confirmation is interrupted, the SDK keeps checking automatically
within the confirmation window; no explicit Check again action is required.
Remove the proposed public .check(), .checkResult() and .recover() methods from
the v1 design. Confirmed success/failure is terminal and must not restart checks.
A fresh invocation is not a safe substitute for obtaining an original outcome.
Confirmation deadline
confirmed 16 October 2026: 30 seconds overall by default, configurable through
defineConfig, measured from initial submission and shared by the initial request
and all automatic receipt checks. Reconnects and checks do not restart that
clock. Expiry settles the handle with a distinguishable confirmation-timeout
error; it does not cancel or imply rollback of server execution. Live queries
continue their independent reconnect behavior. Further connection failures
during receipt lookup do not terminate reconciliation early or restart the
deadline; checks continue with bounded backoff within the remaining budget.
Deadline expiry ends the checks and preserves the distinction between a
confirmation timeout and a confirmed execution failure. Server receipt retention
is derived from the supported confirmation window as specified below; it is not
an independent 24-hour archive.

Receipt retention revised and confirmed 16 October 2026 after excluding reload
recovery: derive completed-outcome retention from the request's supported
confirmation window plus a bounded cleanup grace period. Remove the separate
24-hour default; no extended mutation-result archive is provided in v1.
Configuration of the confirmation window governs the required retention, rather
than requiring users to coordinate two independent durations. Exact grace period
and server-verifiable expiry mechanism require protocol design and verification.
Do not equate client clock expiry with permission to delete active requests or
allow old request identities to execute again. Preserve duplicate-execution
protection independently of full-result cleanup, and cover long-running
transactions, delayed messages, clock differences and configuration changes in
the expiry protocol. Already accepted requests must retain the protection needed
for their accepted window even if configuration subsequently becomes shorter.
Compact completion status confirmed 16 October 2026: preserve a small terminal status
alongside the deduplication metadata, so a known committed request can report
committed/result-expired rather than incorrectly becoming an unknown outcome.
An absent/unverifiable receipt must still not be treated as proof of failure.

No separate 30-day completion-status archive: per-mutation storage scales with
write volume, even when records are small. This is operational recovery/deduplication state, not
an application audit log or a user-event history feature. Keep metadata minimal
and do not retain request inputs, output bodies or diagnostic logs merely to
preserve duplicate protection. Validate storage overhead at realistic write
rates before release; a small per-request record can still accumulate at scale.
For illustration only, 1,000 remote mutation invocations per second produce
2.592 billion records in 30 days; at a hypothetical 100 bytes per record that
is 259.2 GB before indexes, result bodies, replication and write/cleanup costs.
This is not a measured record size or a database-cost multiplier. These receipts
concern remotely invoked mutations, not each SQL statement inside a mutation
or ordinary direct-server database operations.
Permit early full-result cleanup on acknowledgement within the derived receipt
retention window.
Expired outcomes may become unavailable; an expired original request must
still never execute again. Shorter retention reduces resident storage, not the
per-invocation receipt write cost. An alternative is compact per-client sequence
tracking, as in Replicache's reference protocol. That requires explicit handling
of ordering, concurrent requests, gaps and client expiration, and cannot recover
each historical result or success/failure status from a single counter. Do not
adopt that protocol or claim constant total storage without a correctness and
performance design; any remaining result receipts and client records still
need bounds.
Applications needing durable audit history, version history or undo can store
the required prior state or domain events in their own tables, with their own
retention policy. Recording history atomically with the business write is the
application's responsibility. A mutation's returned result alone need not contain
enough information to undo it. Undo after commit is a new transaction subject to
current authorization, constraints and concurrency checks; it must not blindly
overwrite intervening edits. Database rollback on a failed transaction remains
automatic and does not depend on application history or receipt retention.
The bounded-storage mechanism remains proposed: enforce a fixed
server-verifiable validity window on original request identities. Retries must
not extend that window or repackage an old invocation under a fresh identity.
Before metadata can be purged, the server must be able to reject the expired
identity without relying on its deleted marker. A client-provided timestamp or
an absent receipt alone is insufficient. Exact mechanism needs a protocol
design and verification covering concurrent execution, in-flight requests and
configuration changes. Full-result retention must fit within metadata retention;
do not purge active requests. After metadata expiry, report recovery unavailable
and reject execution under the expired identity rather than interpreting it as
a new request. This proposal bounds per-request storage without weakening the
chosen duplicate-execution guarantee.

Early result cleanup confirmed 16 October 2026: retain unacknowledged completed
results for the derived confirmation window and cleanup grace period. Once the
SDK has received an authoritative outcome and settled its in-memory handle,
it can acknowledge receipt so the server can discard the full result early.
A lost acknowledgement leaves ordinary time-based cleanup in place. Retain
the minimal completion/deduplication marker until safe expiry; acknowledgement
must not make the original request executable again. This reduces routine
payload retention without offering durable mutation-result history.

Comparison evidence: Convex documents retrying mutations until confirmation and
exactly-once effects, but the reviewed public documentation does not specify a
numeric result-retention TTL (https://docs.convex.dev/client/react/overview).
Replicache's reference design atomically advances a per-client lastMutationID
and skips already-processed mutations; this is duplicate suppression, not a
documented 24-hour response archive
(https://doc.replicache.dev/byob/remote-mutations). Stripe's idempotency API
permits pruning keys after at least 24 hours, but reusing a pruned key creates a
new request; that latter behavior is not our chosen expiry policy
(https://docs.stripe.com/api/idempotent_requests).

Fullstack frameworks and serverless application hosts must be supported. The
preferred vocabulary is select/insert/update/delete for database access and
query/mutation for named function execution. Declaration syntax was confirmed
on 12 October 2026: use
`query({ input?, output?, handler, ...options })` and the equivalent
`mutation(...)`; `query(handler)` and `mutation(handler)` are exactly equivalent
to `{ handler }`. Handlers receive context first and validated input second.
Generated query/mutation bindings should be directly importable and callable,
without constructing an ordinary-use client or using a separate run method.
Grouping them under an API namespace is optional and belongs to API integration,
not a third shared-client abstraction.

## Target schema validation and inferred types

Decision confirmed 12 October 2026; this section describes the target API, not
currently implemented adapters. Keep `v` for database schema declarations and
accept Standard Schema validators for function input/output. Optional
library-specific adapters produce native, extensible validation schemas; the core
does not depend on Zod.

Use named `createSelectSchema(table)`, `createInsertSchema(table)` and
`createUpdateSchema(table)` exports from `@hulla/db/zod`, with the same factory
names in other adapters. These replace the proposed table-level `toZod` mode
option. Allow field refinement callbacks during conversion. Tables expose the
type-only shortcuts `$inferSelect`, `$inferInsert` and `$inferUpdate`.

- Select schemas describe complete stored rows, including generated fields.
- Insert schemas allow omission of defaulted fields without applying database
  defaults during validation. Generated-field handling follows the table's insert
  key policy; generated-only fields are excluded from writable shapes.
- Update schemas make writable fields optional and do not apply insert defaults.
- Nullable does not imply optional: an insert field without a default still
  requires a value or explicit null unless its generation policy allows omission.

Add a small declarative database constraint vocabulary, including string lengths,
numeric bounds and allowed values. Carry the constraints through the canonical
schema contract and enforce them in storage. Validation adapters preserve their
semantics using native constraints or equivalent refinements; unsupported
conversion must report an error rather than silently discard a constraint.
Function-specific refinements and transformations do not change the table schema.
Generated validators never implicitly coerce values. Authors explicitly opt into
codecs, coercion or transformations through their validation library. Normal SDK
wire encoding and decoding are separate from input coercion.

Support named table checks using a SQL template with schema-aware field references
as an advanced, database-specific escape hatch. Exact check syntax remains a
proposal. Arbitrary SQL checks, uniqueness and foreign keys are database-enforced
and are not automatically converted into standalone function validators.
Standard Schema provides validation/type inference, not a common schema AST.
Shared `.extend` inputs follow the independent-validation and combined-input
contract above; function authors compose library schemas explicitly when needed.

## Common schema and backend writes

A browser-integrated application has schema, query/mutation definitions and client
calls. A server-only application can use the same schema with direct database
operations and transaction callbacks.
Schema declarations and schema-typed callback contexts are implemented; the builder
syntax below still shows its intended shape. The backend-defined
read below is the selected model.

```ts
// db/schema.ts
import { defineSchema, defineTable, v } from '@hulla/db/schema'

export default defineSchema({
  tasks: defineTable({
    title: v.string(),
    completed: v.boolean(),
    ownerId: v.string(),
  }),
})
```

An omitted primary key uses the existing schema contract's generated UUID default.
Authors can still declare another supported primary key, references, defaults and
unique constraints. The generator derives row, insert, patch and table-specific ID
types from this one schema; developers do not maintain a second wire schema.

```ts
// db/tasks.ts
import { query, mutation } from './_generated/functions'
import { z } from 'zod'

export const pending = query(async (ctx) => {
  if (!ctx.identity) throw new Error('Sign in required')
  return ctx.db.select('tasks')
    .where({ ownerId: ctx.identity.id, completed: false })
    .limit(50)
})

export const create = mutation({
  input: z.object({ title: z.string().trim().min(1) }),
  handler: async (ctx, { title }) => {
    if (!ctx.identity) throw new Error('Sign in required')
    return ctx.db.insert('tasks', {
      title,
      completed: false,
      ownerId: ctx.identity.id,
    })
  },
})
```

`ctx.identity` is trusted context supplied by the application's backend integration;
its shape is application-defined. The package does not provide a login service or
require a membership table. Business access rules belong in these functions. The
runtime preserves their snapshot/transaction semantics and reevaluates permission
inputs for live queries.

`ctx.db` is generated and schema-typed. Query functions receive read-only access
within a consistent snapshot. Mutation functions receive read/write access within
one managed transaction, including multiple database operations. The target
contract confirmed 12 October 2026 uses PostgreSQL serializable transactions
for managed mutations and automatically retries the whole operation on retryable
conflicts after confirmed rollback. Handlers must be safe to replay;
external effects stay outside the retried transaction. Ordinary
CRUD does not require caller row versions, though optional original-version or
base-value preconditions can protect stale UI intent. The current generic runtime
still makes serializable execution opt-in. The runtime owns generated IDs,
validation, constraints, recovery receipts and synchronization revisions.
Query/mutation isolation is a target runtime guarantee, including imported
dependencies. Normal calculations and transformations are supported; arbitrary
network access and filesystem writes are unavailable. Runtime controls must prevent
ambient time, randomness or shared mutable state from undermining query tracking
and safe replay. The [restricted native runtime](typescript-functions.md) now
enforces this boundary across bundled helpers and validators. Ordinary server
callbacks are outside this restriction and therefore run once by default.

The existing [transactional outbox](outbox.md) is an optional implementation
capability for applications needing durable records alongside a commit. It is not a
commitment to a general external-job platform, declared external actions or a
scheduling API. External delivery, orchestration and their retry policies belong
to application server code or an appropriate job system.

Missing-row behavior, refined 14 October 2026: filtered updates return an empty
array for zero matches; `.expectRows(1)` makes a missing target an error. An
update never silently recreates a deleted row. Insert-or-update syntax confirmed
16 October 2026: use `insert(...).onConflict(({ on }) => on(...).update(...))`;
do not expose a separate `db.upsert()` method. Recreation requires sufficient
insert data, and schema defaults, constraints and authorization still apply.
When the insert follows a delete in serialization order it can recreate the
row; a later delete can still remove it. Document this as the insert-on-conflict
pattern rather than adding another public write API.

```ts
// application consumer; browser-safe generated bindings for db/tasks.ts
import { pending, create } from './db/_generated/client/tasks'

const tasks = await pending()
await create({ title: 'Ship the SDK' })

const unsubscribe = pending.subscribe((snapshot) => {
  if (snapshot.status === 'success') renderTasks(snapshot.data)
})
// On component/application disposal:
unsubscribe()
```

The definition is authored once. Generation preserves the exported names and
infers the input/result contracts. Ordinary consumers import those functions and
call them; no createClient call, client tree or per-function forwarding wrapper is
required. No-input calls omit an empty argument object. Query bindings additionally
expose `subscribe(callback)` or `subscribe(input, callback)` as a direct convenience
for observing the query handle.

Decision confirmed 13 October 2026: query and mutation calls immediately return
awaitable operation handles exposing execution state, data and errors. Calling
starts the initial execution even if the caller does not await or subscribe. A
non-awaited handle remains supported for consumers that inspect pending state.
Awaiting resolves once to the authoritative result or rejects with the error;
repeated awaits on the same handle do not execute the operation again. This
replaces the proposed `.result` property. Handles can also be subscribed to, with
an initial snapshot and subsequent relevant state/data/error updates. Snapshots
are full state objects, not data-only notifications. An await-only query is a
one-time read; explicit observation keeps it live. Mutations expose the lifecycle
of one invocation, not a continuing database query. Unsubscribing does not undo
or imply cancellation of a submitted mutation.

Mutation recovery states confirmed 16 October 2026 for lookup-only recovery:
the original invocation uses pending, recovering, success and error. Interrupted
confirmation enters recovering while automatic receipt checks run; the same
handle's await remains unresolved. An authoritative recovered result resolves
that await and appears through ordinary success/data. Confirmed execution
failure rejects with its execution error. No separate result accessor, public
unknown status or isRecovering flag is needed. The earlier save.recover(),
save.check() and save.checkResult() proposals are dropped: reconciliation is
automatic within the confirmation window, with no manual recheck API for v1.
Use distinguishable recovery-expired/unavailable DatabaseError codes when a
check cannot establish an outcome. Missing confirmation never establishes
rollback or permission to submit a replacement. Bounded receipt retention cannot
guarantee confirmation after an arbitrarily long absence, and network partitions
can prevent confirmation even while a receipt exists.
Known commit with expired result confirmed 16 October
2026: use `status: "error"` with a distinguishable DatabaseError code (proposed
spelling `RESULT_EXPIRED`) and preserve the confirmed commit information in the
error. Do not add a `resultExpired` status or classify this case as an unknown
execution outcome. Await rejects with that same error; neither the SDK nor UI
should infer rollback or automatically submit a replacement mutation. Exact
error metadata shape remains open. A recovery error without commit evidence
must not carry the confirmed-commit claim used by RESULT_EXPIRED.

Earlier global recovery-list and `createTask.recoverPending()` proposals are
excluded from v1 under the lookup-only, in-memory recovery model. There is no
optional reload-recovery API or browser mutation-persistence layer in v1.

Client query-result ownership confirmed 16 October 2026: expose readonly arrays
and objects, recursively through ordinary nested result structures, in both
awaited query results and subscription snapshots. Consumers must not mutate
shared results; editable drafts require an explicit independent copy, including
copies of nested branches that will be edited. A type assertion alone does not
make an independent copy. Mutations change the database; optimistic updaters
return replacements rather than editing shared query data. Use development-time
mutation checks for ordinary arrays/objects where supported, without requiring
a deep clone on every production read. Readonly typing/freezing of properties
does not automatically make mutable built-ins such as Date internally immutable;
do not promise those runtime checks detect every method-based mutation. Document
copy-before-edit semantics for those values as well.

Framework bindings manage subscriptions, cleanup and reactive rendering; they are
optional. Plain JavaScript applications use subscriptions explicitly. Updating
handle properties alone does not automatically cause a framework to rerender.
Identical queries can share underlying work within their query/input/auth scope.
Decision confirmed 13 October 2026: previously observed queries retain their live
subscription for five minutes after the last observer leaves, with a configurable
duration and bounded idle-cache capacity that can force earlier eviction. Returning
observers cancel expiration and reuse the continuously maintained result. Expiry
releases the subscription and evicts its unused cached result; v1 does not need a
second default timer for disconnected cache retention. Await-only calls do not
start this background retention. Authentication changes and authorization loss
override the timer immediately. This follows Zero's ordinary-query TTL and the
optional Convex cache helper's live-retention default, not TanStack Query's
data-only inactive garbage-collection semantics. Exact capacity limits and public
configuration spelling remain implementation proposals.

Decision confirmed 13 October 2026:
keep freshness and optimism separate from execution state. Snapshots use `status`
(the naming used in the 16 October recovery discussion) for pending/success/error,
with recovering additionally supported for mutation recovery as above,
`isStale` when retained query data is not currently
confirmed fresh, and `isOptimistic` when displayed data includes a provisional
prediction. These indicators may change without the data changing and must notify
observers. An optimistic mutation remains pending or recovering until its
authoritative outcome is known or recovery becomes unavailable; temporary
disconnection need not erase a query's last authorized result.
Authentication scope changes and authorization loss still clear protected data.

Optimistic API clarification confirmed 16 October 2026: `.withOptimistic(...)`
is the sole optimistic API. It predicts configured query results, not the
mutation's return value. Mutation handle data and awaited results are
authoritative server results. Do not add a separate predicted-output API.
Optimistic updater style confirmed 15 October 2026: callbacks receive a current
cached query result and return its replacement, for example
`current => [...current, predictedTask]`. Treat the supplied result as immutable;
do not expose a mutable draft or require proxy/draft machinery. The SDK owns
optimistic layers, rollback and reconciliation rather than asking application
code to save and restore cache snapshots.
Reusable optimistic relationships confirmed 15 October 2026: provide a client-side
function helper/wrapper that configures a mutation's optimistic behavior once,
so components do not repeat cache manipulation at each call. Support multiple
query targets as separate object arguments, without an enclosing queries
property or array. Variadic configuration confirmed 15 October 2026:
mutation.withOptimistic({ query, input, update }, { query, input, update }, ...),
returning a callable mutation wrapper. One target uses one object argument.
Each relationship maps mutation input to query
input and supplies an immutable updater for that query's result. Preserve each
relationship's query-specific input/result inference rather than widening all
targets to one union or any. The helper spelling is `.withOptimistic`.
Per-call override decision confirmed 16 October 2026: do not add an optimistic
opt-out flag or method to the wrapper. Applications can export the original
generated mutation and a separately named optimistic wrapper and choose which
function to call. These exports may share one server mutation declaration;
there is no need to duplicate server logic or introduce another endpoint.
Query-cache prediction does not require or accept a predicted mutation return
value.
These relationships predict query results before commit; they are not a list of
queries to invalidate after commit. Live queries refresh from managed server
dependencies regardless of whether they appear in the optimistic configuration.
Only configured targets receive the explicit optimistic predictions. Reuse the
current auth scope and existing cache identity; never infer membership, sorting
or custom output transformations from matching record IDs alone.
Optimistic query-variant targeting confirmed 15 October 2026: exact query/input
matching remains the default. Include an opt-in matching callback to select
multiple existing cached input variants of the named query within the current
auth scope. Supply the matched query input to the updater so it can respect
filters, sorting and pagination explicitly. Do not widen exact targeting into
all variants automatically or invent results for uncached variants. Recommended
descriptor spelling is match(queryInput, mutationInput), as an alternative to
the exact input mapping, with update(current, mutationInput, queryInput).
No-input query targets omit the input mapper. For input-bearing queries, the
mapper or match predicate supplies selection; a match target needs no unused
mapper. The updater retains its query-specific input and result inference.
Optimistic updater failure policy confirmed 15 October 2026: if an updater
throws, discard that mutation invocation's optimistic query contributions across
all its targets, report the local optimistic-update error and allow the actual
server mutation to proceed. Do not publish a partially applied multi-target
prediction. Apply the same policy if an updater fails during later replay:
remove that invocation's remaining contributions while preserving other
mutations' layers and authoritative server data. Do not convert a local updater
failure into a server mutation failure, retry the mutation because of it, or
report a committed mutation as rolled back. Diagnostic scope confirmed
16 October 2026: provide minimal development-console warnings for optimistic
updater failures. Do not introduce a logging subsystem, stored diagnostic
history, required logger integration or a separate diagnostic subscription API.
Application logging remains the application's responsibility. No dedicated
diagnostic callback is required for this v1 behavior. Warnings should identify
the affected operation without automatically dumping mutation inputs or cached
query data, and should not flood the console during layer replay.
Concurrent optimistic layers confirmed 15 October 2026: keep contributions
separate per mutation and compose them in local submission order over the latest
authoritative query result. If one mutation definitively fails, remove only its
layer and reapply surviving layers; never restore an old whole-cache snapshot
that can erase another mutation's contribution or newer server data. Updater
callbacks may execute repeatedly during reconciliation and must be pure and
deterministic for their supplied input and captured prediction. Create temporary
IDs/timestamps outside the updater; do not perform network calls or external
side effects inside it. Retire successful layers against authoritative revisions
that incorporate their commits, following the reconciliation contract below.
Missing-cache behavior confirmed 15 October 2026: skip a targeted optimistic
query-cache update when that query has no cached server result in the current
authentication scope. Do not invoke the updater with an invented empty value,
create a synthetic query result, or retain that skipped update for later replay
after the first result arrives. The mutation still executes normally; the query
waits for its first server result. A cached [] or
null is a real result, not a cache miss, and is eligible for an updater with
the query's corresponding result type. Authentication changes still fence and
clear the old scope's cache and optimistic contributions.
Optimistic cleanup on transport error confirmed 16 October 2026: remove the
affected invocation's optimistic query layer, just as
for a definitive execution failure. This rolls back local prediction only, not
a potentially committed database transaction. Recompute visible query results
from the latest authoritative base plus surviving optimistic layers; never
restore a whole earlier cache snapshot or discard another invocation's changes.
Previously authorized base results retain normal stale-data rules while offline.
Live queries reconcile with the server on reconnect, so a committed write may
subsequently appear despite the earlier transport error. An explicit outcome
check must not automatically reapply the abandoned prediction. These rules
prevent unconfirmed optimistic state from remaining visible indefinitely or
disappearing only when the application reloads.
Under the later automatic lookup-only reconciliation proposal, preserve this
cleanup at interrupted confirmation even while the handle is recovering. Do
not retain or replay the abandoned prediction solely because receipt checks
continue; authoritative live results remain responsible for query reconciliation.

Decision confirmed 13 October 2026: awaiting a mutation resolves when the server
confirms its transaction committed and supplies the authoritative result. Mutation
success does not wait for affected live-query subscriptions to catch up. Track
query-cache reconciliation separately: retain an operation's optimistic cache
contributions until corresponding authoritative query revisions arrive, rather
than removing them at commit acknowledgement and briefly revealing older data.
A slow or reconnecting subscription must not keep a confirmed mutation pending.

Mutation scheduling comparison 16 October 2026:
Convex documents one ordered mutation queue for calls from its React/Rust clients
(https://docs.convex.dev/functions/mutation-functions#calling-mutations-from-clients).
Zero's current handleMutateRequest implementation processes the submitted batch
sequentially and validates mutation IDs against per-client sequence state,
rejecting out-of-order IDs; this is implementation evidence, not a claim that
all clients share a global queue
(https://github.com/rocicorp/mono/blob/main/packages/zero-server/src/process-mutations.ts).
TanStack Query runs mutations concurrently by default, including repeated calls
to the same mutation, and serializes calls sharing an explicit scope.id
(https://tanstack.com/query/latest/docs/framework/react/guides/mutations#mutation-scopes).
Decision confirmed 16 October 2026: concurrent invocation dispatch,
explicit sequential awaits for application dependencies, and no inferred
per-table queue. Actual read/write sets can depend on inputs and earlier reads;
table overlap is both too coarse for independent rows and insufficient when only
write tables are considered. OCC protects serializable execution, not submission
order or latest-click-wins semantics. Browser handles start on invocation, so
awaiting two already-created handles in sequence does not serialize dispatch.
Separate async event handlers also do not serialize one another merely by each
awaiting a mutation. Optional explicit client queue/scoping support remains a
separate choice; any such queue must account for unconfirmed earlier outcomes.
Sequential client awaits do not make several mutations one atomic transaction.
Scoped queue decision confirmed 16 October 2026: omit an SDK-owned queue API
from v1. Applications may implement their own queues where needed. Document
ordinary sequential invocation, independent concurrent calls,
and single-mutation atomic operations. Any future scoped queue must explicitly
handle confirmation timeout: merely advancing after a rejected client promise
can violate intended ordering if the previous server mutation can still commit.
Client queues would not establish ordering across other clients or replace OCC.

Decision confirmed 13 October 2026: mutations are online-only in v1. Do not accept
new offline mutations into a durable queue for later submission. Report known
unavailability for new submissions; browser online status alone is not proof of
server reachability. This does not remove recovery for already-submitted requests:
if delivery or commit is uncertain, outcome lookup uses the original request
identity rather than assuming failure or replaying it as a new mutation. The v1
SDK does not resubmit mutation payloads after interrupted confirmation; it retries
receipt lookup only, preserving the original identity and authentication scope.
Live queries still
reconnect automatically and may retain previously authorized data marked stale,
subject to auth expiration, scope changes and authorization-loss cleanup. Offline
editing and durable queues for new offline writes are outside v1 scope.

These imports contain callable stubs, public operation identities and types, not
server handler implementations, database drivers or administrative credentials.
Calling `create` dispatches the registered mutation; it does not execute its server
callback in the browser. The SDK handles validation, authentication transport,
snapshot/transaction execution, mutation receipts, synchronization and recovery.
An optional UI adapter owns subscription lifecycle and observes DB state without
requiring applications to parse SSE or manage receipt IDs.

## Import boundaries and runtime configuration

The example uses an explicit generated path to make the browser/server boundary
reviewable. The ideal logical import could instead be `./db/tasks`, resolved to
callable browser stubs. Server code imports ordinary helpers or the server
database API rather than invoking that declaration. Browser imports require an
explicit generation/bundler integration which removes
server implementations and their imports from browser output and verifies the
boundary. Merely declaring a query or placing a file under db/ does not make its
original module safe to import in a browser. Prefer the explicit generated path
until those transforms and conformance checks exist; exact import spelling is open.

Browser direct imports need an application-scoped runtime configured once through
application bootstrap or framework integration. Authentication state and credential
transport follow the explicit boundary above; framework integrations need not
discover the current provider or request automatically. Ordinary server code uses
the server database API and helpers.

Applications use the generated browser setup rather than constructing adapters or
binding contract registries. Server routes and jobs use direct generated `db`
operations, with application-owned authorization and shared ordinary helpers.
See [browser setup](browser-service.md) and [server access](server-database.md).

Importing a function identifies its contract, not a running service's location:

- A same-origin browser runtime can use a mount path recorded once in application
  configuration, for example `/db`, resolving against the browser origin before
  constructing a native Request. Ordinary function calls need no URL argument.
- Remote deployments configure a public `baseUrl` through environment-specific
  runtime/transport configuration. Never expose a PostgreSQL connection string
  or administrative credentials in generated browser artifacts.
- Ordinary server database access configures its service connection separately.
  Browser-relative paths alone do not identify remote services for SSR, native
  applications or command-line tools.
- Advanced multi-deployment/multi-account use needs explicit bound scopes or an
  optional client factory, preserving separate resources and authorization state.
  Such an escape hatch must not become the mandatory ordinary-use API. Connection
  configuration should reuse the API package's transport/baseUrl vocabulary where
  applicable; exact interoperability remains open.

Changing accounts must cancel old work, retire old watches and clear scoped state
through the SDK lifecycle, even when the consumer retains an imported function.
Direct imports do not make that lifecycle optional. Browser SDK access does not
grant table or admin access: the service validates inputs and enforces function
permissions. Any future client-authored table reads require the explicit server
read scopes and coverage rules below.

A DB-only application can invoke functions and subscribe directly through Hulla's
authenticated service. It needs no application API server or @hulla/api client.

## Optional API colocation

Using both packages does not require a shared client. Import the DB functions and
API operations independently where that fits the application. If a developer wants
one namespace, colocate selected DB bindings under the application's API entry
module. This is optional API integration, not a third client layer:

```ts
// api/client.ts — browser-safe application module
import * as tasks from '../db/_generated/tasks'
import { billing } from './billing/client'

export const api = { tasks, billing }
```

```ts
import { api } from './api/client'

await api.tasks.create({ title: 'Ship the SDK' })
const unsubscribe = api.tasks.pending.subscribe(renderTaskState)
const checkout = await api.billing.checkout({ body: { plan: 'pro' } })
if (checkout.status === 200) openCheckout(checkout.body.url)
// On disposal:
unsubscribe()
```

Here billing is an ordinary selected API client. The tasks branch reexports the
same DB callables, preserving their runtime, inferred types and watch capabilities.
No separate createTask route/handler or mutation wrapper is authored. Explicit
branch names avoid silently replacing an operation. An API integration generator
could emit this namespace or expose the same DB definitions through generated
HTTP contracts/hosting adapters. Exact API contract registration syntax is open;
the current API core does not automatically accept DB definitions or add watches
to HTTP client leaves. The object above is ordinary module composition, not an
implemented server-side contract adapter.

Preserve execution and state ownership with or without the API namespace:

- Each DB function has one implementation. Optional HTTP exposure derives from
  that definition, preserving authorization, transaction boundaries, mutation
  request identity and recovery; it does not duplicate business logic.
- The DB runtime owns live results and mutation recovery. API/UI integrations
  observe that state rather than add competing polling caches, generic retries
  or a second optimistic write layer around managed DB operations.
- Application session configuration has one source, with credentials appropriate
  to each service's origin/audience. Session changes retire old DB state and
  invalidate relevant HTTP caches through their owning framework. Grouping
  functions into an object does not itself implement that lifecycle.
- Server calls remain request-scoped, and watches/cancellation retain their
  resource lifecycle. Colocation of source or names does not grant admin access.

Ordinary API routes retain their declared status/body/header unions; DB functions
retain typed function values and operation failures. Grouping functions does not
silently convert those contracts. An application may explicitly map selected API
responses into domain values/errors, keeping raw HTTP contracts available; blanket
status unwrapping or discarded headers is not an integration default.

Standalone DB and API packages remain independently usable. Their optional
integration belongs to the API boundary and needs no shared-client SDK.

## Operation vocabulary and execution semantics

Use `select`, `insert`, `update` and `delete` for database primitives. Invoke named
functions directly through their generated callable bindings, and use a query's
`subscribe` method to observe its result. Names such as `pending`, `create`, `complete`
or `transfer` belong to the application. The exact builder signatures remain
provisional; a separate `run(reference, input)` method is unnecessary for ordinary
application calls.

A named function can contain multiple database primitives. For example, `complete`
can select a task, check ownership, update it and insert an audit row in one
transaction. Calling such a function an `insert` would describe only one of its
steps. Ordinary server/admin database operations should also be available without
requiring a named function for every statement.

The runtime still needs an explicit distinction between read-only and transactional
execution. Read functions are eligible for dependency tracking, caching and watches;
declared mutations commit atomically and use request/receipt recovery. The
`query(...)` and `mutation(...)` wrappers describe that execution boundary.
They do not require applications to manually manage snapshots or BEGIN/COMMIT.
Runtime data access should use the same operation verbs, including `update` for a
partial field update rather than introducing a separate `patch` lexicon.

Do not infer execution kind solely from whether a handler happened to write in one
invocation. A branch may write only for different inputs; a cached/watched call
must never perform writes. Generated types should give reads a read-only database,
reject watching a transactional operation and preserve the execution kind through
framework/transport adapters. External-service work belongs in ordinary application
backend code, outside declared queries and mutations.

### Framework integrations

Scope confirmed 12 October 2026: support React, Vue, Svelte, Solid and Astro, plus
meta-framework integrations including Next.js, Nuxt, SvelteKit and SolidStart.
Framework-specific server directives are not core requirements. The shared SDK
owns transport, live-result state and mutation recovery; UI bindings connect
framework reactivity and lifecycle; server integrations handle request identity and
SSR. An Astro page using multiple UI frameworks should use the corresponding UI
bindings for its islands without a second core client implementation. Exact package
layout remains open.

Hulla supplies database/function execution and transport. User framework server
actions and API routes own external-service work and may use direct server database
operations and ordinary shared helpers. They are not imported into restricted
query/mutation handlers. Framework integrations remain thin bridges to existing
framework facilities and preserve validation, authorization and request identity.

## Target table naming

Naming and registration decision confirmed 13 October 2026: retain the explicit
`defineSchema({ ...tables })` registry. Its object keys are the authoritative
database table names, independent of TypeScript variable names, preserving casing
and spaces supported by the storage adapter. For example,
`defineSchema({ "User Profiles": users })` registers the `users` table definition
under the database name `User Profiles`. Both `v.id(users)` and
`v.id("User Profiles")` reference that table; string-based database calls use the
same name. Do not add a separate `tableName` override, self-naming table overload,
or table-discovery fallback. Tables may live in separate modules and be imported
into the explicit schema registry. Object references resolve through registered
table identity; JavaScript variable names do not determine database names.
Quote physical names as identifiers, validate adapter restrictions and reject
ambiguous registrations. This changes the author-facing storage mapping; do not
assume the existing generated physical table names already implement it.

## Target references and related reads

Decision confirmed 13 October 2026: use `v.id("table")` for references to another
table's primary key, with `.nullable()` for optional references. Also support
`v.id(tableDefinition)` with the same deletion options and nullability. Object
references resolve through the table's registered schema identity, not its
JavaScript variable name. Both forms produce equivalent foreign-key enforcement
and generated table-specific ID types. String references are an equally supported
way to avoid self-reference or mutual initialization/type-inference cycles; check
their targets against the assembled schema. Resolve target
tables and key types from the complete schema; generated ID types distinguish
tables. Enforce references as real database foreign keys. This is target syntax,
not a claim that the current `v.uuid().references("table")` implementation already
provides it. Cross-table name/type validation must be verified in the type and
schema-compilation prototypes rather than assuming standalone strings have full
schema autocomplete.

V1 composes ordinary database reads inside query handlers and shared helpers.
Do not add `.include()`, `one`/`many`, or named relationship declarations for v1.
Reads inside a declared query share its consistent snapshot and contribute to its
reactive dependencies. Collection reads compose through membership predicates in
the ordinary filtering API rather than a separate `getMany` API for v1.
Many-to-many data uses explicitly declared linking tables and
appropriate uniqueness constraints, not hidden relationship tables.

Deletion policy confirmed 13 October 2026: restrict deletion of referenced rows by
default. Allow explicit per-reference cascade and set-null options in v1; set-null
requires a nullable field. Enforce these rules in database storage for declared
mutations and direct server writes alike. Cascade or set-null changes occur in
the same transaction, and must participate in managed invalidation/accounting so
dependent live queries refresh correctly. Exact option spelling remains proposed
(`onDelete: "restrict" | "cascade" | "setNull"`). Nested implicit writes or
automatic object persistence are not introduced by reference declarations.

## Target filtering expressions

14 October 2026 filter revision: use where(field, operator, value) and
equality-object notation. Field references describe paths, not expression methods;
do not implement the earlier field.eq(value) design.

Predicate composition revised 14 October 2026: retain simple positional
comparisons and equality-object notation; use a conventional expression builder
in `.where(q => ...)` for complex conditions. This replaces the callable group
builder and fluent mixed and/or chains explored earlier. Do not implement the
variadic SQL-token alternative, parenthesis strings, or the proposed whereNot,
andNot and orNot query methods. A callback constructs and returns a predicate;
it does not execute a query. Documentation uses destructured helpers, such as
`.where(({ or, eq }) => or(eq(users.role, "admin"), eq(users.role, "moderator")))`,
rather than a repeated q prefix. Helpers must work when destructured, without
depending on a JavaScript this receiver. Naming the callback argument q remains
valid user code, not a separate API.

Concrete shape confirmed 14 October 2026: and(...), or(...) and
not(expression), with comparison helpers such as eq(users.role, "admin").
The nested expression tree defines grouping explicitly, so there is no fluent
AND/OR precedence policy to learn. Repeated `.where(...)` calls combine
their complete predicates with AND, including callback and object predicates.
Exact helper names and string-field overloads remain to be settled.

Empty compound predicates confirmed 15 October 2026: and(...[]) produces true
and or(...[]) produces false. Apply these logical identities when conditions
are constructed dynamically, and preserve them under nesting and negation.
An actual undefined entry is invalid and must error, rather than being silently
removed. Empty groups are valid expressions, not absent filters. This does not
remove the requirement to supply where explicitly on update/delete operations.

Null comparison policy confirmed 14 October 2026: equality-object null values
mean IS NULL. Positional comparisons use (field, "is", null) and
(field, "is not", null); callbacks use isNull(field) and isNotNull(field).
Equality and inequality with a null value operand are also supported: normalize
(field, "=", null) to IS NULL and (field, "!=", null) to IS NOT NULL. Apply the
same normalization to callback eq(field, null) and ne(field, null); ne is the
proposed inequality helper spelling. Explicit is / is not operators and
isNull / isNotNull helpers remain available. Check the actual operand value,
including a runtime variable containing null, rather than only literal source
syntax. This normalization does not make ordinary non-null comparisons or
field-to-field comparisons null-safe; those and negation retain SQL null semantics.

Text matching confirmed 14 October 2026: support contains, startsWith and
endsWith for literal text, and like / ilike for intentional SQL wildcard
patterns. Percent and underscore characters in literal matching have no wildcard
meaning; parameter binding alone does not escape pattern metacharacters, so
adapters must preserve literal semantics explicitly. Expose matching helpers
in the callback expression builder as well as positional operators.

Literal text matching case policy confirmed 14 October 2026: contains,
startsWith and endsWith are case-sensitive by default. Allow an optional
{ caseSensitive: false } argument rather than separate insensitive helper names:
`.where("name", "contains", input, { caseSensitive: false })` and
`.where(({ contains }) => contains(users.name, input, { caseSensitive: false }))`.
Omitting the option or passing true preserves the default. The option changes
case sensitivity, not literal-versus-pattern interpretation. Keep the agreed
explicit like / ilike pattern operators. Locale/collation semantics and whether
to support this option for other string comparisons remain separate decisions.

JSON array membership confirmed 14 October 2026: support
`.where("tags", "contains", "urgent")` and
`.where(({ contains }) => contains(tasks.tags, "urgent"))` in v1. The field's
declared type determines contains semantics: literal substring matching for a
string, exact whole-element membership for an array. Match array elements by
whole-value equality, including objects and nested arrays; do not interpret an
object operand as a partial-object pattern or use recursive containment in place
of exact element equality. Validate operands against the declared element type
and applicable codecs. Case-sensitivity options belong to string matching,
not array membership.

Array filter scope expanded and confirmed 14 October 2026: include contains
(one exact element), containsAny (at least one supplied element), containsAll
(every supplied element), and containedBy (every stored element belongs to the
supplied list). Expose equivalent positional operators and callback helpers.
These membership operations ignore ordering and duplicate counts, while retaining
exact whole-element equality. Whole-array = / != comparisons use structural
equality with ordering and duplicates significant; = [] provides an empty-array
check. Include the callback expression arrayLength(field), returning a numeric
expression usable by ordinary comparison helpers, for example
`.where(({ gte, arrayLength }) => gte(arrayLength(tasks.tags), 3))`.
Defer arbitrary any/all element predicates with their own element scopes to v2.
Nullable-array and empty-list behavior confirmed 15 October 2026: an actual
empty array has arrayLength 0; a missing/null array produces null length and
unknown for array membership/containment predicates, including under negation.
For actual arrays, containsAny([]) is false, containsAll([]) is true and
containedBy([]) is true only for an empty stored array. Preserve null behavior
even when the supplied list is empty; scalar IN [] constant semantics do not
define array-column containment behavior. Explicit null checks can include
rows with missing/null arrays.

Null array elements confirmed 15 October 2026: contains(field, null) tests for
an actual null element inside an array, provided the element schema permits
null. It must not normalize to isNull(field). Null elements supplied to
containsAny, containsAll and containedBy follow the same exact element-equality
rules. A null or missing array remains distinct from an array containing null
and retains the unknown predicate behavior above. Validate element nullability
in types and at runtime; undefined elements remain invalid filter operands.

JSON path existence confirmed 15 October 2026: expose an exists operator and
callback helper, rather than the previously proposed hasPath name. Presence
must remain distinguishable from an explicit JSON null. Use
`.where("settings.theme", "exists", true)` (or false),
and `.where(({ exists }) => exists(users.settings.theme))`, with not(...) for
negation. A path exists when its final property is present even if its value is
JSON null; an absent or null intermediate parent makes the child path absent.
Presence shorthand confirmed 15 October 2026: `.where(path, "exists")` means
`.where(path, "exists", true)`, and `.where(path, "not exists")` means
`.where(path, "exists", false)`. This is an operator-specific arity overload,
not a two-argument equality shorthand. Retain the boolean argument on exists
for dynamic conditions. Keep is / is not null spelling unchanged. A negated
presence predicate tests absence, not whether a present value is null.
Null-check policy confirmed 15 October 2026: isNull / equality
with a null operand matches missing paths, explicit JSON null and SQL-null
parents; isNotNull / inequality with a null operand matches only present,
non-null values. Keep identical behavior across positional, object and callback
filters. This convenience is not the literal PostgreSQL JSON extraction
IS NULL behavior. It does not change ordinary non-null comparisons into
null-safe comparisons or introduce an undefined filter operand. Document the
equivalences together: = null, is null, equality-object null and isNull match
missing or null paths; != null, is not null and isNotNull match only present,
non-null values. An exists check combined with a null check selects explicitly
present JSON null. Include the missing/null/non-null truth table in public docs
so users can see how presence and normalized comparisons interact.

Positional negation confirmed 15 October 2026: keep negation in the second
argument's operator string, not in a separate argument. Support applicable
forms such as "not contains", "not containsAny", "not containsAll",
"not containedBy", "not between", "not like", "not ilike",
"not startsWith" and "not endsWith", alongside the already agreed
"not in", "not exists", "!=" and "is not". Keep optional matching options
in the fourth argument rather than shifting them for negated operators.
Expose field-type-appropriate literal operator suggestions in IntelliSense,
including negated variants. Do not accept arbitrary strings as operators.
Normalize generic negated predicates through a shared NOT expression; retain
the explicitly specified null-normalization and existence semantics of their
specialized operators. Callback expressions use not(expression), without
requiring a separate helper for every negated operation. Preserve SQL unknown
under negation for ordinary comparisons and nullable-array predicates.

Range filters confirmed 14 October 2026: (field, "between", [lower, upper]) and
the callback helper between(field, [lower, upper]) include both endpoints.
Require exactly two bounds with types compatible with the field. Do not sort or
swap reversed bounds; they match no rows. Exclusive and half-open ranges use
explicit ordered comparisons, such as >= start and < end. Ordinary SQL null
semantics still apply to nullable stored fields.

Empty membership lists confirmed 14 October 2026: (field, "in", []) produces
a false predicate; (field, "not in", []) produces a true predicate. Apply this
uniformly to positional and callback expression forms, including dynamically
constructed arrays. These predicates must compose normally under AND, OR and
NOT; do not simply omit an empty membership predicate from its enclosing group.
The constant result also applies when the field value is null. Do not generate
invalid SQL IN () syntax.

Null membership normalization confirmed 14 October 2026: allow null list items
for nullable fields. Split null from non-null operands: IN [values..., null]
means (field IN (values...) OR field IS NULL); NOT IN [values..., null] means
(field NOT IN (values...) AND field IS NOT NULL). A list containing only null
reduces to IS NULL / IS NOT NULL, without generating an empty SQL IN clause.
Apply this equally to positional and callback filters and runtime array values.
Lists without null retain ordinary SQL membership semantics; in particular,
nonempty NOT IN lists without null do not implicitly include nullable rows.

Undefined filter operands confirmed 14 October 2026: reject undefined rather
than omitting a condition, converting it to null, or returning a constant
predicate. Apply this uniformly to positional comparisons, equality-object
entries, callback comparisons, membership list elements and range bounds.
Reject statically where possible and validate runtime values for JavaScript and
type-bypassing callers. Optional filters must be included explicitly through
ordinary application control flow. This does not change the separate write
policy where undefined update properties mean unchanged.

Field-to-field comparison direction confirmed 14 October 2026 after checking
established APIs: support compatible same-table generated field references as
comparison operands in both positional where and callback helper forms.
`.where(tasks.completedAt, ">", tasks.dueAt)` and
`.where(({ gt }) => gt(tasks.completedAt, tasks.dueAt))` have identical meaning.
Do not introduce the explored callback-only restriction. Strings in value
positions always mean literal values, never inferred field names. Generated
references are identifiable expression objects, not ordinary row values; validate
their scope and operator/storage-type compatibility in types and at runtime.
Comparisons between fields operate on stored values, without decoding either
column into an application value or encoding one column through another's codec.
Documentation must show adjacent examples of a literal value and a generated
column reference in the operand position, explaining that references represent
columns rather than the current row's application values. Never infer a column
reference from a string operand that happens to match a column name.
Reference: Drizzle's documented eq/ne/gt column operands
(https://orm.drizzle.team/docs/operators) and Convex FilterBuilder's
ExpressionOrValue operands on both sides
(https://docs.convex.dev/api/interfaces/server.FilterBuilder).

Table/field references and mixed access confirmed 14 October 2026: select accepts
the schema table name or a generated query-facing table reference. Comparisons accept string
field paths or table-bound field references, such as
users.settings.appearance.theme. Allow mixing them in one query. Equivalent
forms normalize to the same query meaning, but separate builder objects retain
separate execution identities; do not promise Object.is equality or automatic
execution deduplication. Reject unrelated-table references outside an explicitly
supported cross-table scope. Dotted strings represent nested field paths.
Literal-dot property access confirmed 14 October 2026: use generated references
with ordinary bracket access for keys containing dots, such as
users.settings["appearance.theme"]. Each property access preserves its exact key
as a path segment; never reconstruct reference paths by joining and splitting
on dots. Thus "settings.appearance.theme" traverses three keys, while the
generated reference above traverses two. Do not add a separate path-array
syntax in v1. Apply this distinction consistently wherever field paths are
accepted, including filtering, selection and ordering.

String path scope confirmed 14 October 2026: paths are always relative to the
selected table; do not accept an optional table-name prefix or infer qualification
from matching names. In a users query, "age" selects the age column, while
"users.age" selects the age member of a JSON column named users. Generated
references supply explicit table identity, such as users.age versus users.users.age.
Autocomplete must not duplicate paths with prefixed and unprefixed forms. Future
join/alias scopes require a separately explicit qualification design.

Table definition/reference separation clarified 14 October 2026: defineTable
returns the schema-construction builder, with ordinary methods such as .index().
Generated query-facing tables are separate objects exposing typed column/path
references. Do not put $meta on defineTable to access schema builder methods and
do not require query callers to import executable schema-construction builders.
Their column declarations are arguments to defineTable, so a column named index
does not collide with the builder's .index() method.

Generated table import location confirmed 14 October 2026: expose query-facing
table references through _generated/tables, separate from _generated/server's
database instance and _generated/functions' declaration constructors.

Generated table helper namespace direction: put any public table-reference
metadata/helpers under one explicitly reserved property, with $meta the proposed
spelling. Keep column paths free of query/schema methods. The proposed reservation
is at the generated table root only; nested JSON keys need no such namespace.
Validate reserved-name collisions rather than silently hiding a column. Exact
namespace name, contents, and reservation scope await final
confirmation. This namespace does not relocate the schema builder API.

Explicit comparison operators confirmed 14 October 2026: positional comparisons
require (field, operator, value), including equality via "=". This is the simple
where overload; complex callback expressions use the separate expression-builder
API described above. Do not provide the two-argument
equality overload. This keeps the operator visible and its completion position
separate from arbitrary operand values. A literal string "LIKE" is compared as
where("name", "=", "LIKE"), while pattern matching is
where("name", "LIKE", "S%"). Keep the separately requested equality-object form,
such as where({ active: true }); removing positional shorthand does not remove
object notation. Validate argument counts and operator eligibility at runtime as
well as through generated types. Do not implement completion-preserving string
union workarounds for the discarded optional-operator form.

Boolean construction policy retained from 13 October 2026: JavaScript `&&`, `||` and `!`
do not construct database expressions: expression objects are truthy and
short-circuiting loses conditions. Do not introduce a compiler transform or
expression-recording mechanism to reinterpret those operators. Ordinary JavaScript
control flow may choose which expression to construct using application values.

The proposed operator set covers equality, ordered comparisons, membership,
inclusive ranges, null checks and literal text matching. Compatible field-to-field
comparisons follow the policy above; remaining exact operator names and SQL-pattern
operator syntax still require final confirmation. Literal matching and SQL
wildcard matching must remain distinct; never silently interpret user text as a
pattern. Equality-object shorthand and repeated-where AND semantics follow
the confirmed composition policy described above.

SQL scope confirmed 13 October 2026: offer an optional tagged SQL expression
escape hatch over the current query's fields for v1; defer arbitrary statements
and subqueries in this API. Interpolated fields are quoted identifiers and
interpolated application values are bound parameters. Expressions remain inside
the caller's transaction, read/write capabilities and execution limits. A tag or
TypeScript annotation alone does not validate SQL, establish reactive dependencies,
or prevent side effects through database functions. Before implementation, specify
and verify enforced expression restrictions, including function eligibility and
dependency tracking. Reject unsupported expressions rather than silently weakening
query purity, OCC replay safety or live-result correctness. Database-specific
expressions do not acquire cross-adapter portability through this escape hatch.

## Target field selection

Decision confirmed 13 October 2026: `db.select("table")` selects full rows by
default. An optional second argument selects fields, either as an array of field
names or an object inclusion map. Keep selection with the table argument rather
than adding `.pick()` or using `.returning()` for reads. Filtering and ordering
may reference fields absent from the result projection.

Support conditional array entries such as `includeDescription && "description"`.
Ignore only `false`, `null` and `undefined` entries, not arbitrary falsy values.
Object values are `true` to include a field, or `false`, `null` or `undefined` to
exclude it. Runtime conditions are supported. Reject unknown field names and
invalid selection values statically where possible and at runtime for JavaScript
or type-bypassing callers.

Infer literal inclusions as required result properties and literal exclusions as
absent properties. Fields included conditionally at runtime have optional result
properties. Preserve the field's own nullability independently of whether it was
selected. Excluded properties are absent, not returned with undefined values.
Reject an empty effective selection; never interpret it as selecting every field.
V1 object selection is an inclusion map only: aliases and computed SQL result
expressions are deferred. Query handlers may construct derived public output.
Field selection is implemented across TypeScript inference, generated function
contracts and PostgreSQL execution as of 20 October 2026. SQL reads fetch only
selected columns plus private pagination boundary fields. See
[server field selections](server-database.md#return-only-the-fields-you-need).

## Target insert results

Decision confirmed 14 October 2026: inserts return full inserted rows by default,
including primary keys and database-applied defaults. Support optional
`.returning(selection)` to narrow the returned fields, using both the array and
object inclusion syntax established for reads. Reuse its conditional inclusion,
field validation, inferred optionality and empty-selection rules rather than
introducing a separate projection language. Returning a narrowed result does not
change the inserted values.

Bulk insertion confirmed 14 October 2026: overload the same `.insert(table, data)`
method for a single object or an array; do not add `.insertMany()`. Object input
returns one row object. Array input returns an array even when its runtime length
is one. Preserve this input-type-based shape with `.returning(...)` projections.
The entire batch is atomic: any failed insert rolls back all inserts in that batch.
An empty input array returns an empty result array without issuing a write. Do not
silently chunk a batch into independently committed transactions. Validation and
resource limits still apply. Bulk-result ordering confirmed 14 October 2026: do
not guarantee input order or add correlation/reordering work solely to preserve
it. Applications should correlate returned rows by stable identifiers or unique
keys when needed. Sorting by timestamps may provide useful presentation order,
but does not prove input order: timestamps can tie and conflict updates may refer
to preexisting rows. Preserve the separate deterministic ordering contract for
ordinary reads.

## Target insert conflict handling

Direction confirmed 14 October 2026: allow `.onConflict(...)` as a callback with
schema-narrowed eligible conflict targets. Targets must correspond to actual
supported primary-key or unique constraints, not arbitrary columns or every kind
of database constraint. Composite uniqueness applies to the complete field tuple;
membership in a composite constraint does not make a field independently unique.
Validate target eligibility at runtime as well as through generated types.

Conflict handling never disables database rules. Any inserted or updated state
must satisfy applicable constraints, including foreign keys, non-nullability,
checks and other uniqueness rules. The callback builds an atomic conflict policy,
not an arbitrary JavaScript error handler replayed after a failed insert.

Conflict targeting confirmed 14 October 2026: align with PostgreSQL by selecting
one supported primary/unique key per insert conflict policy, which may be a
single-column key or a complete composite key. Do not introduce ordered fallback
handlers across independent unique keys. Different keys can identify different
existing rows; the SDK must not choose an identity or merge rows implicitly.
An update resolving the selected conflict still fails if it violates another
constraint. The intended actions are an explicit update patch or skipping the
proposed insert while leaving the existing row unchanged. This never permits an
ignore-all-errors switch.

Conflict selector syntax confirmed 16 October 2026: use a callback exposing an
`on` helper for both single and composite keys, documented with destructuring:
`.onConflict(({ on }) => on("email").update({ name: newUser.name }))` or
`.onConflict(({ on }) => on("organizationId", "userId").skip())`.
Field names are arguments, not properties competing with helper names. Limit
accepted fields/tuples and autocomplete to eligible declared primary/unique keys;
reject incomplete composite targets through types and runtime validation. Do
not add property selectors such as c.email.update or a different composite-only
selector.
Conflict update values confirmed 16 October 2026: accept either a plain patch
object or a callback `({ current, incoming }) => ({ ...patch })`. Both references
are schema-typed database expression references, not fetched JavaScript objects.
`current` addresses the existing conflicting row; `incoming` addresses the row
proposed for insertion (the SQL excluded-row concept). In bulk inserts each
conflict uses its own incoming row. For example,
`.update(({ current, incoming }) => ({ name: incoming.name,
updateCount: current.updateCount.add(1) }))`. Compile the callback as an atomic
conflict-update expression in the insert statement; do not run a client-side
read/modify/write loop. Preserve normal assignment typing, codec/storage
semantics and database constraint enforcement. Plain patches such as
`.update({ active: true })` remain supported for both single and bulk inserts.

Conflict result semantics confirmed 14 October 2026: return only rows actually
inserted or updated by the operation. A successful single insert or conflict-update
returns the written row; a skipped single insert returns null. Choosing a skip
policy introduces that nullability into the inferred result type, including with
`.returning(...)`; ordinary inserts still return a row or throw. Bulk inserts
return an array containing only inserted/updated rows, omitting skipped entries
without placeholders or fetching unchanged existing rows. The result may be
shorter than the input. This settles skip behavior; it does not establish positional
correspondence or adapter-independent return ordering for bulk results.

## Target write targeting

Decision confirmed 14 October 2026: updates and deletes require an explicit
`.where(...)`. Target selection belongs in that predicate, not a positional target
argument. Intentional table-wide writes use `.where(() => true)`; do not introduce
a redundant `.all()` method. Support constant boolean predicates as well as typed
field expressions. Enforce the presence of `.where(...)` through the builder types
and at runtime before execution. This catches omitted predicates; it does not
prove that a supplied predicate is restrictive or authorized. Do not silently
insert a true predicate for callers that omit `.where(...)`.

Write syntax confirmed 14 October 2026: use
`db.update("tasks", { ...changes }).where(...)`, consistent with insert values in
the second argument. Do not introduce `.set()`. Update values are a partial patch;
omitted fields remain unchanged. Delete targeting uses
`db.delete("tasks").where(...)`.

Update expressions confirmed 14 October 2026: the second update argument may also
be a callback returning the patch, for example
`db.update("posts", post => ({ likes: post.likes.add(1) })).where({ id })`.
The callback builds schema-typed database expressions; it does not receive fetched
row values or execute per row in JavaScript. Evaluate expressions against existing
row values in the database statement, within the caller's transaction and limits.
Support compatible field references in assignments. Keep expression operations
property-first rather than adding separate database increment/decrement methods.
Literal object patches remain the simple default. Exact arithmetic operator scope,
numeric result/overflow rules and interactions with nullable fields still require
the numeric contract; never imply arbitrary JavaScript operators are translated.

Update/delete results confirmed 14 October 2026: return full affected rows by
default, always in an array even when exactly one row matches. Updates return
post-update values; deletes return the deleted rows' pre-deletion values. Both
support `.returning(selection)` with the same array/object projection and
conditional-inclusion rules as inserts and reads.

Affected-row policy accepted 14 October 2026: ordinary filtered updates/deletes
allow zero matches and return an empty array. Offer `.expectRows(n)` as an optional
advanced helper for an exact affected-row requirement, not required ceremony in
ordinary examples. Check the expectation inside the transaction; a mismatch throws
DatabaseError and prevents the transaction from committing, including if the
application catches the error. Callers
that require an existing row explicitly assert the expected count. Never infer
strictness from a predicate's shape or silently recreate a deleted row through
update. Prioritize core writes before this convenience.

Affected-row refinement confirmed 14 October 2026: preserve PostgreSQL update
semantics. An executed update counts and returns its affected row even if assigned
values equal the previous values. An explicit conflict skip returns no row and
contributes nothing to the affected-row count. `.expectRows(...)` checks affected
rows, not differences between old and new values; `.returning(...)` projection
does not change that count. Authors can explicitly predicate on old values to
avoid updates they do not want to execute.

Expectation callback confirmed 14 October 2026: retain `.expectRows(n)` as
exact-count shorthand and accept a synchronous JavaScript predicate over the
actual affected-row count, for example
`.expectRows(count => count >= 1 && count <= 10)`. Do not introduce a numeric
expression builder here. Unlike `.where(...)`, this callback evaluates a known
scalar after the statement executes and before the transaction commits; it needs
no SQL translation or compiler transform. Require a boolean result and reject
async/Promise results statically and at runtime rather than treating them as
truthy. False, thrown errors or invalid results prevent commit even if caught.
The predicate runs again for each attempt of an automatically retried mutation and
inherits its replay/side-effect restrictions. It is not a row limit and must never
truncate a write to satisfy the expectation.

Expectation coverage confirmed 14 October 2026: expose the same exact-count and
synchronous-predicate overloads across insert, update and delete, including bulk
inserts and insert conflict policies. Apply them consistently on direct server
database handles, transaction handles and writable mutation contexts. Count the
rows affected by the requested write statement: successful inserts and conflict
updates contribute, explicit skips do not. Bulk writes check the total for the
operation, not an independent expectation per input item. Keep count behavior
independent of `.returning(...)` selection and scalar/array result shape. This
uniformity does not add write capabilities to read-only query contexts.

## Target single-row reads and aggregation

Decision confirmed 13 October 2026: `.limit(n)` always returns an array, including
literal or runtime limits of one. Do not make its return shape depend on the
limit's value. Leave the `.first()` convenience out of v1; callers can destructure
the array. Keep `.unique()` as a distinct terminal operation: return the matching
row, null for no match, and throw DatabaseError for multiple matches. It checks
query cardinality and does not create a storage uniqueness constraint.

Aggregation scope confirmed 13 October 2026: include count, sum, minimum, maximum
and average in v1. Do not automatically compute total matching counts for ordinary
or paginated results. Returned-row counts are already available from array length;
an exact total over all matches can require substantially more database work.
Aggregate execution should remain in the caller's snapshot, reactive dependency
tracking and execution limits, without materializing every matching row into
JavaScript.

Aggregate syntax confirmed 13 October 2026, with grouped composition refined
14 October 2026: expose one
`.aggregate(a => ({ ...namedResults }))` method rather than top-level aggregate
methods on the query builder. The callback offers `a.count()`, `a.sum("field")`,
`a.min("field")`, `a.max("field")` and `a.avg("field")`, with schema-typed fields
and operation-appropriate field types. Result keys are author-defined and inferred;
compute multiple requested aggregates in one database statement. Ungrouped
aggregation always returns one result object, including for a single aggregate.
Helper names are separate from field names, avoiding collisions with columns named
count, sum or min.

Empty-set/null behavior confirmed 13 October 2026: preserve SQL semantics.
`a.count()` counts matching rows and returns zero when none match. Field aggregates
ignore null input values and return null when there are no non-null inputs,
including sum. Do not silently substitute zero; authors may explicitly use a
fallback.

Count representation confirmed 14 October 2026: aggregate row counts and
`.expectRows(...)` callback counts are JavaScript numbers. Require nonnegative
safe integers and reject counts beyond Number.MAX_SAFE_INTEGER rather than
silently rounding. Field representation follows the separately selected
modes/codecs below.

Numeric aggregate representation confirmed 14 October 2026: ordinary number-mode
fields return number from sum and avg. Lossless bigint-mode integer fields return
bigint from sum and a decimal string from avg, since averages may be fractional.
The string preserves the database numeric result, not a promise of mathematically
exact representation for repeating fractions. Reject number-mode integer sums
outside JavaScript's safe integer range instead of rounding. Preserve SQL null
results for empty/all-null aggregate inputs. Authors may convert decimal strings
into decimal-library objects in application code or output transformations.
Custom-codec aggregate mappings require their own contract; do not apply an
arbitrary field codec to a sum or average automatically.

Field representation direction confirmed 14 October 2026: provide compact
built-in representation modes for common conversions and an advanced
`.codec({ encode, decode })` for custom conversions. Bind representation to the
field schema so inferred read/write types and schema adapters can reflect it,
rather than relying on distant runtime configuration. The proposed numeric
default is ordinary JavaScript numbers, with explicit lossless modes such as
`v.int64({ mode: "bigint" })`. Number-mode integers must remain safe integers;
reject out-of-range values instead of silently rounding. Additional numeric
types and custom-codec aggregate mappings still require a complete contract.

Codec contract: decode the database representation on reads and encode the
application representation on writes. Built-in modes and custom codecs share
the conversion machinery. Validate storage constraints against encoded values;
this mapping does not introduce implicit coercion into function input validators.
Encode field-comparison operands through the field codec. Codecs must be
synchronous and deterministic. Database comparison, ordering and aggregation
retain storage semantics; arbitrary codecs do not redefine SQL operations.
Do not indiscriminately apply a field codec to aggregate results, which may have
different types or ranges. A storage codec alone does not define a transport codec.

Codec comparisons and ordering confirmed 14 October 2026: keep equality and
range comparisons available according to the underlying database type, encoding
supplied application values before comparison. Ordering uses stored values and
the database's comparison rules. Do not guess comparison semantics from the
decoded JavaScript type or require codec flags asserting order preservation.
For example, a numeric column decoded to Money sorts numerically, while a text
column decoded to a number still sorts as database text.

Codec text-pattern operands confirmed 14 October 2026: contains, startsWith,
endsWith and like on stored text accept storage-level strings directly, bypassing
the whole-value codec. Complete-value comparisons such as eq still encode the
application value. A fragment or wildcard pattern is not necessarily a valid
complete application value, so do not try to encode it with that codec. Bind
these operands as parameters, preserving each operator's literal-fragment versus
wildcard-pattern semantics. Document this distinction with a text field decoded
to URL: eq accepts a URL, while contains accepts a stored-text fragment.

Codec transport boundary confirmed 14 October 2026: direct server database calls
may return custom decoded objects. Browser query/mutation results must use
supported transport values; authors convert custom objects in the handler or
output schema, for example a Money instance into an amount/currency object.
Do not automatically reconstruct arbitrary classes or introduce a shared custom
transport-codec registry in v1. Reject unsupported output values rather than
silently dropping their data or pretending their prototypes survive transport.

Codec null handling confirmed 14 October 2026: null passes through unchanged
without invoking encode or decode. Nullable fields expose DecodedType | null;
codec callbacks handle non-null values only. Nullability remains a schema
concern and is still enforced on writes. Null predicates retain SQL semantics.
Omitted write properties remain omitted, preserving database defaults on insert
and unchanged fields on partial updates; omission must not invoke a codec.
Application fallbacks for null belong in handlers or output transformations.

Date/time representation confirmed and refined 14 October 2026: distinguish
instants from calendar dates and timezone-free clock readings. `v.timestamp()`
defaults to timezone-aware storage (PostgreSQL timestamptz) and JavaScript Date;
`v.timestamp({ mode: "string" })` exposes a normalized UTC ISO timestamp string.
Timezone-aware storage represents an instant, not the original timezone name.
`v.date()` exposes a validated YYYY-MM-DD calendar-date string without attaching
a time or timezone. Allow `v.date({ mode: "date" })`: decode to midnight UTC and
encode the Date's UTC calendar date, discarding its time component.

Allow `v.timestamp({ withTimezone: false })` for timezone-free timestamps, with
string representation by default. An explicit mode: "date" opts into a UTC
mapping convention: interpret stored clock components as UTC when decoding and
store the Date's UTC clock components when encoding. This convention does not
give the stored value timezone semantics. withTimezone controls database
semantics; mode controls application representation. Never implicitly use the
machine's local timezone. Document that local-time formatting of calendar-date
Date values can display a different calendar day. Do not add a separate datetime
helper for this behavior.

Built-in Date transport must preserve the Date application type across server
and browser query/mutation results; this does not extend to arbitrary custom
classes. Custom field codecs remain available.

Timestamp precision confirmed 14 October 2026: default storage precision to
milliseconds (precision: 3), including string mode and timezone-free timestamps.
Allow explicit finer precision with string representation or a suitable custom
codec. Reject precision above milliseconds combined with built-in Date mode,
including when Date mode is selected by default. Report an explanatory schema
configuration error that identifies the field and recommends string mode, a
suitable codec, or lower precision. Reject statically where possible and always
validate at runtime during schema construction/registration, before executing
queries. Do not silently discard sub-millisecond precision on Date conversion.

JSON fields confirmed and refined 14 October 2026: support unrestricted
`v.json()`, type-asserted `v.json<T>()`, and structured, schema-validated JSON in
v1. The generic supplies the application type without runtime shape validation;
it is a developer assertion, not a guarantee that stored values match T. All
three forms check JSON compatibility. The proposed structured shorthand is
`v.json({ theme: v.string(), notifications: v.boolean() })`, reusing field
validators to infer the application shape and validate writes through the
database API. Unrestricted JSON still requires valid JSON values; reject
unsupported values such as functions and circular references rather than
silently serializing away data. Document the distinction between the generic
type assertion and runtime shape validation. JSON query operators and
enforcement for writes outside the SDK remain to be
specified; this decision does not promise native database enforcement of every
nested validation rule.

JSON update semantics confirmed 14 October 2026: retain partial updates at the
row/column level, but replace a supplied JSON column's entire value. Omitted
columns remain unchanged. Do not implicitly deep-merge objects or arrays.
Validate the complete replacement against its declared JSON schema when present;
generic-only JSON declarations still check compatibility without shape validation.
Defer dedicated nested JSON update helpers beyond v1. Authors may read, modify,
and replace JSON within the same mutation or server transaction, subject to that
scope's concurrency and retry guarantees; separate unprotected reads and writes
must not be presented as safe from lost updates.

Comparison references: Convex db.patch shallow-merges document fields, replacing
supplied nested objects (https://docs.convex.dev/database/writing-data). Drizzle
ordinary update assignments replace supplied column values; explicit SQL
expressions are a separate capability (https://orm.drizzle.team/docs/update).

Update omission/null semantics confirmed 14 October 2026: an omitted top-level
update property or an explicitly undefined value leaves the column unchanged.
An explicit null writes null only when the field is nullable; otherwise reject
the write. Do not introduce an unset sentinel or a third absent-column state for
fixed-schema rows. Ignore undefined update properties before invoking field
codecs. This omission rule is not recursive: undefined inside a supplied JSON
value remains invalid JSON and must not be silently stripped. Removing an
optional JSON object key requires replacing the JSON value with a valid value
that omits the key, not an unset operation on the database column.

Structured JSON unknown properties confirmed 14 October 2026: reject undeclared
properties with a validation error identifying the offending property path.
Apply this at each schema-declared object boundary; do not silently strip or
preserve unknown keys. Unrestricted v.json() and type-asserted v.json<T>() retain
JSON-compatibility checks without imposing a runtime object shape. Explicitly
open object/record schemas, if introduced, require their own declared contract.

Nested JSON schema helpers confirmed 14 October 2026: support explicit
v.object({ ... }) and v.array(elementSchema) within validated JSON, including
array roots such as v.json(v.array(v.string())). These describe values within
one JSON column, not additional database columns or tables. Validate each array
element and retain strict declared-object boundaries.

JSON storage versus shape confirmed 14 October 2026: retain v.json as the
explicit column storage boundary. v.object and v.array describe value shapes
inside JSON schemas; they do not independently select column storage. Preserve
the compact object shorthand v.json({ ... }) rather than requiring an extra
v.object wrapper. Array roots use v.json(v.array(...)); unrestricted and generic
roots retain v.json() and v.json<T>(). Do not replace these with implicit JSON
storage for standalone object/array declarations. Document nested object shapes
with v.object, keeping storage and shape roles distinct; redundant nested
v.json declarations are not a confirmed v1 requirement.

Documentation requirement confirmed 14 October 2026: explicitly explain the
object shorthand in a note alongside the JSON schema examples in the public
documentation, rather than leaving readers to infer it:

> **Note:** `v.json({ ... })` is a convenience shorthand for
> `v.json(v.object({ ... }))`. Both declare one JSON column with the same strict
> object validation; the shorthand lets you omit the `v.object()` wrapper.

Show array roots separately as v.json(v.array(...)) so this convenience does
not imply that all root schemas use the object shorthand.

JSON conversions confirmed 14 October 2026: require explicit application
conversion or an explicitly declared codec for non-JSON application values such
as Date and bigint inside JSON. Do not infer string conversions, invoke toJSON
implicitly, or introduce automatic tagged representations. For example, a
v.string().codec({ encode: (value: bigint) => value.toString(),
decode: (stored: string) => BigInt(stored) }) inside a structured JSON schema
stores a JSON string and explicitly reconstructs a bigint on reads. Encoded
values must satisfy the underlying schema and JSON compatibility checks.
Unrestricted v.json() and generic v.json<T>() have no inferred codecs from T
and reject unsupported runtime values. Decoding custom values does not override
the separately defined browser transport boundary.

Grouping confirmed 14 October 2026: support `.groupBy("field")` before
`.aggregate(...)`, including multiple grouping fields via
`.groupBy("status", "ownerId")`. Grouped aggregation returns an array of flat
objects containing both the automatically included, schema-typed grouping fields
and the author-named aggregate results. No matches produce an empty array.
Reject aggregate aliases that collide with grouping field names rather than
overwriting either value. Do not wrap group keys and aggregate values in separate
nested objects.

Grouped composition confirmed 14 October 2026: grouped `.aggregate(...)` returns
an awaitable, composable builder rather than being strictly terminal. Awaiting it
directly executes the grouped query and returns the array; authors may instead
chain `.orderBy("aggregateAlias", "desc")` and terminal `.limit(n)` before
execution. Ordering fields are typed against the group-result shape. These
operations order and limit groups, not the input rows. Input `.where(...)`
conditions apply before aggregation. Complete group ordering with the grouping
fields as deterministic tie-breakers, rather than the original rows' primary keys.
Ungrouped aggregation still yields one summary object. Grouped cursor pagination
is deferred to v2, confirmed 14 October 2026. V1 supports reactive grouped results
with ordering and limits, but does not expose `.paginate()` on grouped builders.
Ordinary row pagination remains supported. A group limit bounds returned groups,
not necessarily the input rows or aggregation work. Future grouped pagination
must define stable boundaries and coordinated publication when aggregate values
move groups between pages or changes create/remove groups.

Group-result filtering confirmed 14 October 2026: expose explicit `.having(...)`
after grouped `.aggregate(...)`, rather than changing `.where(...)` to mean group
filtering. The callback uses the same property-first predicates and explicit
boolean composition, typed against the group-result fields and aggregate aliases.
Authors may refer to a named aggregate without repeating its expression. Compile
those references to valid adapter expressions; do not assume every SQL dialect
permits select-list aliases directly in HAVING. `.where(...)` filters input rows;
`.having(...)` filters computed groups before group ordering and limits.

## Target pagination boundary

Decision confirmed 13 October 2026: pagination belongs to database logic inside
ordinary query handlers. Authors choose fixed limits or explicitly declare cursor
and limit values in their input schema. Do not inject pagination into every query,
add a query-level `pagination` declaration option, introduce `ctx.pagination`, or
reserve client invocation options for automatic pagination. Non-paginated queries
remain unaffected. Author-declared pagination input participates in cache identity
like other input. No helper should assume every function result is a page.

Database helper separation confirmed 13 October 2026: `.limit(n)` independently
returns a bounded array of rows. `.paginate({ limit, cursor })` explicitly requests
a page with rows and continuation metadata; omit the incoming cursor for the first
page. Do not introduce a separate `.cursor()` builder method for v1. The paginated
return shape must not depend on whether an incoming cursor exists. Both helpers
share underlying query execution where appropriate. Exact page metadata remains
to be settled. The returned continuation
field is named `cursor`, not `nextCursor`; it is passed as the next request's cursor.
Both `.limit(n)` and `.paginate({ limit, cursor })` are terminal operations. Their
return types expose no query-builder modifiers, including either terminal method,
so chaining them in either order is unavailable in autocomplete and rejected by
TypeScript. Filters and ordering precede the terminal operation. JavaScript or
type-bypassing misuse must also fail clearly at runtime; never silently combine
limits or choose an order-dependent precedence rule.

Live pagination confirmed 13 October 2026: `limit` sets the initial page size;
observed pages retain stable cursor boundaries and may grow or shrink as matching
rows change. Do not refill each page to its original count after live updates.
Standalone `.limit(n)` continues to impose a strict maximum on its live result.
Await-only pagination remains a one-time read. Stable boundaries do not freeze row
membership: changes to sort values or filters may move rows between ranges. The
SDK must coordinate related page updates to avoid transient gaps or duplicates in
a managed live list. Resource bounds and page splitting require an implementation
design; stable ranges must not imply unlimited growth or memory usage. Exact
client coordination and resource-limit APIs remain open.

Managed live pagination refined 16 October 2026: provide an optional live-list
helper over an existing paginated query, with an explicit mapping for cursor
input. Construction syntax confirmed 16 October 2026:
`listTasks.paginate(cursor => ({ projectId, limit: 20, cursor }))`. Expose this
method only for generated queries returning the SDK's pagination result type.
The mapping produces ordinary schema-validated query input; it does not reserve
input field names or require the query to expose a page-size argument. Ordinary
`listTasks(input)` calls still return a single page. Server builder paginate
syntax is unchanged. The helper exposes a single readonly loaded-row
snapshot and releases related page subscriptions together on disposal. Use
`.next(pages = 1)` to append additional live pages, not replace existing rows.
Require a positive integer, fetch sequentially as each cursor becomes available,
stop at exhaustion and make further calls harmless no-ops. Expose hasMore.
Overlapping next calls share the in-flight load rather than accumulate additional
requests; the first call determines that load's page count.
Loading state refined 16 October 2026: do not add separate isLoading and
isLoadingMore flags. Use the ordinary pending status for initial loading and
explicit next-page loading. Preserve loaded data while requesting more pages;
initial pending has no result yet, whereas pending with existing data is further
loading. Ordinary reactive row updates do not repeatedly put the list into
pending. Append failure behavior confirmed 16 October 2026: retain successfully
loaded pages and their subscriptions, expose error status with the append error,
and reject the active next() call. A later next() resumes from the last
successfully loaded cursor. If next(n) loads some pages before failing, preserve
those successful additions as well; do not restart from the original cursor or
publish a failed page. An append error does not invalidate otherwise valid
loaded-page results. This differs from a handler error in an already-active page
query, which follows the existing query error/data-retention contract. Auth-scope
changes and authorization failures still take precedence over data retention.

Pagination input changes confirmed 16 October 2026: changing filters, sorting or
other non-cursor inputs creates a new managed list starting at its first page.
Release the old list's subscriptions; never append pages from the new inputs to
the old list. Captured inputs in the cursor-to-input mapping must remain stable
for the lifetime of its handle. Framework integrations can manage replacement
and cleanup when those inputs change; plain JavaScript callers unsubscribe and
create a new handle. Do not infer dependency tracking for arbitrary closures or
silently reuse an old cursor across different query conditions.

Ordering confirmed 13 October 2026: use primary-key ordering by default. Authors
may choose ordering properties and directions explicitly. Complete a non-unique
ordering with the primary key as a deterministic final tie-breaker, without
overriding explicit ordering choices. Default key order does not imply creation
order. Automatic deterministic ordering does not imply automatic index creation.

Index policy confirmed 13 October 2026: v1 uses explicit, typed index declarations
in the table schema. The database planner selects indexes automatically; ordinary
queries do not need to name an index. Create declared indexes through explicit
schema application/deployment, never as a side effect of executing a query.
Automatic index-generation tooling is deferred beyond v1. Future recommendations
may combine static query shapes with execution-plan diagnostics and generate
reviewable schema changes; arbitrary TypeScript helpers and dynamic branches
prevent exhaustive static inference.

Index declaration syntax confirmed 13 October and retained 14 October 2026: chain
`.index(["projectId", "createdAt"])` on `defineTable(...)`, with field names typed
against the table schema. Generate a deterministic name when omitted; accept an
explicit name as a second options argument, for example
`.index(["projectId", "createdAt"], { name: "tasks_by_project_created" })`.
Ordinary query filters and ordering do not reference these index names. Direction,
null-ordering and advanced index options still require a detailed contract.

Execution-limit policy confirmed 13 October 2026: allow queries without a suitable
index, subject to bounded execution. Absence of an index is not itself an error;
small-table scans can be appropriate. Development console diagnostics should warn
when an execution approaches its configured limits and identify the consumed
budget. Exact limits, measurement semantics and warning thresholds remain
proposals. A result-row limit alone does not bound database scan work.

Limit configuration confirmed 13 October 2026: provide package defaults through
`defineConfig` and optional partial overrides on the final query/mutation object
declaration. Omitted settings inherit the configured defaults; trusted server
declarations may raise or lower individual limits. Do not expose limit options on
`.extend()` or accept overrides from browser invocation input. Validate settings
when configuration/declarations load. Apply warnings against the effective limits.
Server execution timing excludes browser request upload and response delivery;
client transport timeouts and mutation outcome recovery are separate concerns.
Payload defaults confirmed 13 October 2026: independently allow up to 16 MiB of
serialized function input and 16 MiB of serialized public output per invocation.
Both limits are configurable globally and through final-declaration overrides.
Measure uncompressed encoded bytes. Check submitted input before schema validation
and handler execution, including extension input in the same submitted payload;
check output after output validation/transformation and before mutation commit.
Apply the output ceiling separately to each live result, not cumulatively over the
subscription lifetime. Input/output budgets are distinct from database-read
budgets. Other numeric defaults and exact option names remain provisional. Ensure
bounded transport handling accommodates the effective payload limits and protocol
overhead rather than accidentally imposing a smaller cap or buffering without a
bound before checking input size.

These are target APIs, not claims about the current implementation.

## Target coordinated live-query publication

Decision confirmed 13 October 2026, subject to validating a bounded implementation:
coordinate observed query publication within one client runtime, database source
and authentication scope. Successful current results in a published batch must
represent one consistent database state. This extends to related pagination
pages. Mutation await completion remains server-commit-confirmed and does not wait
for subscription publication. Cross-tab, cross-client and cross-database atomic
publication are outside this guarantee.

The existing implementation uses separate per-query read transactions and
deliveries; revision tags alone are not a substitute for shared-snapshot execution
or a proof that a cached result remains valid at the target state. The initial
implementation candidate is a bounded read-only snapshot round followed by atomic
client cache replacement before observer notification. Capture subscription-set
membership for each round; fence obsolete rounds after auth/source changes and
discard incomplete publication after disconnect. Coalesce intermediate revisions
without indefinitely restarting a round under continuous writes. Idle retained
queries must not hold up the actively observed set. Framework adapters must
preserve the cache's consistent publication semantics.

Prototype snapshot sharing, execution/resource bounds, cancellation, subscription
changes and reconnect behavior before claiming this guarantee. Ordinary query
errors can be batch outcomes, whereas infrastructure failures may require
discarding the entire round. Do not relabel old successful data as current to let
a batch advance.

Refresh-failure defaults confirmed 13 October 2026: retain the previous coherent
view as stale during connection or temporary infrastructure failures, while still
authorized. On an ordinary handler error, publish that query's error and clear its
previous data by default, together with other successful results from the round.
Authentication or authorization loss clears affected data immediately.

Handler-error retention confirmed 13 October 2026: support the subscription
option `{ keepPreviousDataOnError: true }`, passed after the listener in
`query.subscribe(input, listener, options)` (or the corresponding no-input/handle
subscription overload). Revised later the same day: permit an application-wide
default through `defineConfig`, with per-subscription overrides taking precedence.
The package default remains clearing data on handler error. Do not place this
presentation option on server query/mutation declarations. Retain only a previous successful result for the
same input and auth scope, locally to the subscribing consumer. Its snapshot has
`status: "error"`, the current error, retained data and `isStale: true`; retained
data is outside the current batch's consistency guarantee. The authoritative
cache remains in the error state. Auth/access failures override retention and
clear affected data. Awaited calls still reject.

Handler-error recovery confirmed 13 October 2026: ordinary handler errors do not
terminate the live subscription. Preserve sufficient dependency information from
failed executions to reevaluate on relevant subsequent database invalidations and
return to success when execution succeeds. A recovery policy must account for
failed executions with incomplete dependency discovery.

Recovery/retry distinction confirmed 13 October 2026: reacting to relevant database
changes remains intrinsic to an active subscription, including after handler
errors or exhausted timed retries. Do not introduce a separate `recoverOnChange`
option for v1. Configurable `retries` controls additional timed attempts after a
failed execution, independently of transport reconnect. `retries: 0` disables
those additional attempts, not live invalidation. A later change-triggered
execution may begin its own bounded retry sequence; coalesce overlapping work
rather than starting competing loops. Global subscription defaults and individual
subscription overrides use the same precedence as error-data retention. Exact
handler retry default confirmed 15 October 2026: retries: 0. Timed handler
retries remain opt-in globally or per subscription. Query connection timeouts are
eligible for automatic transport recovery independently of this setting;
retries: 0 must not disable reconnect or recovery of interrupted delivery.
Keep connection failures distinct from a server handler execution timeout,
which is an execution-limit error and does not receive this transport exception.
Mutation delivery uncertainty enters lookup-only reconciliation under the latest
16 October proposal; automatic receipt checks use the original request identity
without resubmitting a mutation. Lookup deadline expiry is a communication error,
not proof of server rollback. Exact transport backoff and
handler retry eligibility remain to be specified; do not infer mutation retries
from query-subscription settings.

One-shot query deadline confirmed 15 October 2026: default to a configurable
30-second overall caller deadline, including connection recovery, rather than
allowing an awaited query to wait indefinitely. Transport attempts share that
deadline; reconnecting does not reset it. Keep this client-observed elapsed-time
limit separate from the server execution budget. Deadline expiry rejects the
one-shot operation with a timeout error; it must not terminate shared live
subscriptions. Active live subscriptions continue reconnecting while subscribed,
retaining only previously authorized data under the existing stale-data and
auth-clearing policies. This query deadline does not establish a mutation
rollback/failure policy or require automatic mutation recovery.

One-shot query cancellation confirmed 15 October 2026: accept a standard
AbortSignal in an optional call-options argument, for example
getTask({ id }, { signal: controller.signal }). Aborting rejects that call and
releases its consumer interest without cancelling shared work still needed by
other callers or subscriptions. Stop unneeded local delivery/retry work; server
execution cancellation is best-effort, not a guarantee of immediate termination.
Do not expose this as mutation cancellation in v1 or imply that cancelling
communication rolls back a submitted mutation. Standard subscription teardown
remains separate from cancellation of a one-shot call.

## Target function error contract

Decision confirmed 13 October 2026: successful generated calls resolve directly to
their result; failures reject/throw rather than returning a mandatory data/error
envelope. Normalize input validation issues into structured SDK errors with field
paths and messages, and expose stable error codes. Explicit client-visible
application errors are distinct from unexpected server exceptions, whose details
remain in server diagnostics rather than being sent to callers.

Use one public SDK error class, `DatabaseError`. Developers declare intentional
client-visible failures with `new DatabaseError(...)`; do not add a `.public`
factory, visibility option or separate application-error class. Its documented
transport payload is safe for caller consumption by contract. SDK-created
DatabaseErrors must follow that same contract; native SQL/driver diagnostics and
causes remain server-local. Ordinary unexpected exceptions are sanitized at the
remote boundary, not made public merely because they were thrown.
Constructor confirmed 16 October 2026:
`new DatabaseError({ code: "TASK_LOCKED", message: "This task can no longer be edited.",
details: { taskId } })`. Require code and message; details is optional serializable
data. Preserve these fields across the remote error boundary so callers can
branch on codes rather than parsing messages. Detailed SDK code vocabulary and
special recovery metadata remain to be specified; constructor shape is settled.

Result transformations use ordinary handler return expressions or an explicit
Standard Schema output validator/transform. Apply the output schema to successful
handler results, infer the exposed result from its output type, and validate the
transport result before a mutation commits. Output transformation does not handle
thrown errors. Do not add a separate result-transform hook for v1.

## Function declaration syntax

Decision confirmed 12 October 2026: declare functions with
`query({ input?, output?, handler, ...options })` or the equivalent
`mutation(...)`. Both support optional validated input. The handler
receives the typed context first and parsed input second. Passing just a handler is
exactly equivalent to passing `{ handler }`, for functions
without input or other options. The input and output schema fields are optional;
output types are inferred from the complete handler return expression, including
derived expressions. Generated runtime validation checks supported serializable
return values. An explicit output schema adds compile-time and runtime checks.
The declared-mutation OCC retry count is three retries after the initial attempt,
as specified above; option names and exact randomized backoff intervals remain open.
`db.transaction(callback)` is a server API, not a standalone function declaration,
and its callback runs once by default.
Positional validator-plus-handler forms and builders are not part of this API.

This separates application input/output validation from invocation/transaction
plumbing. The initial generator discovers named exports and emits operation
identities and typed callable bindings. Server dispatch uses the original
declarations and schemas. Schema-derived server registration/deployment artifacts
remain work. Binding an operation changes its invocation syntax, not its execution
guarantees: direct calls still pass through validation, authorization,
snapshots/transactions and mutation receipt recovery. There is no separate client
type declaration or route tree to maintain.

## Rust authoring and direct imports

This section inventories the implemented native registered-call API. Its scoped
call model remains documented as current behavior; it does not add declaration
invocation to the intended server/helper API above.

The native `hulladb` crate implements table/input derives, query/mutation
attributes on ordinary async functions, typed snapshot/transaction contexts,
direct async calls and query watch namespaces such as `pending::watch()`.
See [Rust authoring](rust-authoring.md) for supported attributes, key policies,
codecs, authorization, execution scopes and recovery limits. The
[executable tasks example](../crates/hulladb/examples/tasks.rs) exercises this API
against an owned PostgreSQL namespace.

Declare rows with an explicit primary key; derives emit canonical metadata, typed
field selectors, insert/patch inputs and checked value codecs:

```rust
use hulladb::{Id, Input, Table};

#[derive(Table)]
#[table(name = "tasks")]
pub struct Task {
    pub id: Id<Task>,
    pub title: String,
    #[field(default = false)]
    pub completed: bool,
    pub owner: String,
}

#[derive(Input)]
pub struct CreateTask {
    pub title: String,
}
```

`hulladb::schema!(Task)` normalizes this through the existing schema contract.
`TaskInsert` omits the generated key and uses `Option<bool>` for the defaulted
completed field. `TaskPatch` excludes the key and distinguishes omitted fields
from explicit null. Named unique constraints, references, literal defaults,
custom key types/generators and explicit-key policies retain canonical semantics.

Attributes adapt a typed handler to managed registration and a public input-only
call signature:

```rust
use hulladb::{mutation, query, Error, MutationContext, QueryContext};

#[query(name = "tasks.pending")]
pub async fn pending(ctx: QueryContext<'_, String>) -> Result<Vec<Task>, Error> {
    ctx.db.select::<Task>()
        .where_eq(Task::fields().owner, ctx.identity.clone())
        .where_eq(Task::fields().completed, false)
        .take(50).await
}

#[mutation(name = "tasks.create")]
pub async fn create(
    ctx: MutationContext<'_, String>, input: CreateTask,
) -> Result<Task, Error> {
    let title = input.title.trim().to_owned();
    if title.is_empty() {
        return Err(Error::InvalidInput("Title required".into()));
    }
    ctx.db.insert(TaskInsert {
        title,
        completed: None,
        owner: ctx.identity.clone(),
    }).await
}
```

The identity shape is application-defined. Query contexts expose only snapshot
reads; mutation contexts expose managed transaction reads/writes. Table IDs and
field values are checked by Rust's type system, and installed schema metadata is
checked before typed database execution. Decoders reject unknown/missing fields
and preserve UUID/int64 wire semantics.

Register operations with `pending::register(&mut registry, policy)` and
`create::register(&mut registry, policy)`. A mandatory typed authorization policy
must also authorize receipt recovery; handler-only business checks do not run
when serving an existing receipt. The resulting registry works with the generic
HTTP/SSE host or an embedded `LocalRuntime`.

Inside an established `hulladb::scope(runtime, future)`, consumers import the same
public functions and input structs:

```rust
use app::tasks::{create, pending, CreateTask};

let tasks = pending().await?;
let task = create(CreateTask { title: "Ship it".into() }).await?;
let subscription = pending::watch().await?;
drop(subscription);
```

Rust permits `pending()` and the same-named `pending::watch()` module, including
through imports. Mutation namespaces have no watch function. Owned subscriptions
release their polling task on drop. Scopes bind fixed verified identities; calls
outside a scope fail and child Tokio tasks require explicit scopes. Retiring the
old runtime rejects calls and clears its watches before an identity switch.
Registered calls inside handlers/policies fail; compose transaction logic with
ordinary helpers and the existing database context.

The initial `LocalRuntime` is an embedded/server PostgreSQL adapter with polling
watches and an in-memory request journal. It recovers uncertain requests with
the same request identity, but does not provide generic native HTTP transport,
a durable journal or automatic watch resubscription. The optional
[Rust remote runtime](rust-remote-client.md) adds verified HTTP/SSE, SQLite result
and intent persistence, reconnects and client-only macro emission without
PostgreSQL/server dependencies. Cross-language signature generation remains further work. TypeScript clients can
invoke these registered handlers with equivalent public contracts. TypeScript
callbacks can also execute through the optional [transaction worker](typescript-functions.md),
with native authorization and managed recovery.
Framework request scopes and native admin transactions are implemented in
[production operations](production-readiness.md).

## Fullstack frameworks and serverless applications

The standard deployment, confirmed on 12 October 2026, is the supplied Hulla DB
service. It discovers and runs schemas/functions and owns native hosting, worker
management, registration and subscriptions. TypeScript authors do not write Rust
host registration or transaction-worker scaffolding. Framework and serverless
application code calls this service through the SDK.

Server and browser code can live in one application repository. Generated modules
must separate public callable bindings/schema types from server handlers,
direct database access, database drivers and administrative credentials. Framework
adapters should enforce server-only imports during builds, not rely on directory
names as a security boundary.

A server loader, React Server Component or framework server function uses ordinary
server CRUD and application authorization. If it shares database logic with a
browser declaration, extract an ordinary helper accepting the required database
capability. The example below uses the existing illustrative query builder; server
connection bootstrap remains under design:

```ts
import { db } from './db/_generated/server'

// The application verifies the request and authorizes this owner before calling.
function pendingTasksForOwner(ownerId: string) {
  return db.select('tasks')
    .where({ ownerId, completed: false })
    .take(50)
}
```

The application owns request identity and authorization in this server path. The
database handle contains no user identity. Transport resources and connection pools
can be reused across requests; transaction handles and request results remain
scoped to their work. Browser declarations enforce their own ordinary guards at
the mounted, credential-verifying endpoint.

The browser calls the generated binding through its client SDK. SSR can pass a
scoped initial result through a framework adapter, then the browser attaches a live
watch. That bootstrap must preserve operation/argument/authorization identities
and avoid treating an SSR snapshot as proof of current live subscription state. A changed
user must not reuse another user's bootstrap/cache. Framework page/data caches and
the SDK's live cache have separate lifecycles; adapter invalidation/hydration support
is required rather than assuming one cache automatically updates the other.

| Deployment | Handler and runtime arrangement |
| --- | --- |
| Persistent fullstack application | Application handlers may use a colocated managed runtime or a separate database service. Direct server operations use application authorization and managed database boundaries. |
| Serverless application using Hulla service | SSR/actions are short-lived application entry points. The SDK performs ordinary database operations against the persistent transaction/sync service. Browser watches can connect to that service through its authenticated transport. |
| Application-hosted functions in serverless invocations | Requires a transaction bridge with ownership, cancellation, deadlines and disconnect rollback. This is a distinct runtime adapter, not a guaranteed property of the current HTTP bridge. |
| Fully serverless Hulla runtime | Requires durable/shared subscription coordination and a host-supported live-delivery transport. The existing in-process scheduler/feed cannot simply be placed in every ephemeral request. |

Start serverless support with serverless application callers and registered database
functions deployed near the managed Rust runtime. This keeps the application DX
small without requiring long-lived synchronization workers in framework requests.
Do not force a separate backend repository: schema and function source can stay in
the fullstack application and be bundled/deployed by generation/development tooling.
Application-hosted function execution can be added through the explicit transaction
adapter once its guarantees are established.

Browser integrations invoking declared mutations must preserve the SDK request
identity across forwarding/recovery rather than turn recovery into a new write. Every
server entry point still verifies application identity; being exported with a
framework server directive does not by itself establish authorization. Shared
connection pools may be reused, but identity, transactions and request results are
request-scoped. Persistent progress belongs in authoritative storage, and request
completion must not be the only mechanism advancing subscription visibility.

Relevant framework/runtime references:
[Next.js server/client boundaries](https://nextjs.org/docs/app/getting-started/server-and-client-components)
and [Lambda execution-environment lifecycle](https://docs.aws.amazon.com/lambda/latest/dg/lambda-runtime-environment.html).
These describe host behavior; the Hulla adapters above are proposed, not implemented.

## Server and admin access

Ordinary application backends use a server-only database entry point configured
once, with schema-typed `select`, `insert`, `update`, `delete` and
`db.transaction(callback)`. Exact initialization and method signatures remain under
design. Grouped callbacks execute once by default and surface transaction conflicts.
Declared functions instead receive the scoped `ctx.db` described above. Reusable
DB-only helpers accept that context so declared callers retain their snapshot,
transaction and tracked reads.

Direct server access is a normal usage mode, not inherently administrative access.
Administrative privilege controls which data/operations a trusted server can access;
it does not bypass storage constraints, transaction integrity, revision tracking or
applicable recovery. All committed server writes remain visible to subscriptions.
Clients never receive the direct database module or its credentials. Server callers
use CRUD or ordinary helpers with the least required capability; helpers join the
provided scope. Declarations supply browser entry points and their guards.

A small server configuration supplies storage; application server code owns its
authentication and authorization. The mounted browser endpoint verifies credentials.
The SDK/runtime handles function registration and serving. `@hulla/api` integration
is optional: generate its contracts/routes from the registered functions and mount
through an adapter. Applications should not recreate each route and response schema
or deploy a second hand-written task API merely to use the database.

## Package and generation responsibilities

| Application declares | SDK / generator implements |
| --- | --- |
| Table fields, keys, references, defaults and constraints | Canonical schema contract, storage mappings, validation and generated types |
| Function arguments, execution kind and handlers | Typed contexts, registration, dispatch and callable public bindings |
| Verified identity and business access rules | Scope isolation, tracked permission inputs and checks during live delivery |
| Connection/auth configuration | Wire codec, request identities, deadlines, transport and recovery |
| Function calls and result watches | Scoped result identity/completeness, cold-read loading, shared live results, freshness, synchronization and bounded resource lifecycle |
| Optional API adapter mounting | Generated route contracts, input/output codecs and HTTP/stream serving |

Generating route boilerplate alone is insufficient. The underlying runtime must
actually execute schema-typed functions with the promised read/write semantics.
`hulladb_postgres::managed` now supplies the generic storage/function boundary using
canonical records and separate read-only/read-write contexts. The
[deployed adapters](sdk-adapters.md) connect generated TypeScript callables to
registered native handlers, including browser persistence and managed recovery.
The [Rust authoring API](rust-authoring.md) now adds typed contexts and registration
adapters. [Production operations](production-readiness.md) adds framework/admin
integration and deployment tooling. The optional [TypeScript worker](typescript-functions.md)
executes callbacks through the same transaction boundary.

## Implementation order

The [replacement delivery plan](sdk-implementation-plan.md) supersedes the earlier
foundation sequence. It specifies correctness proofs, reuse/removal boundaries,
ordered implementation packets, agent ownership, executable application acceptance
and documentation validation. Read that plan before assigning implementation work.
Legacy integration fixtures may supply useful failure-injection scenarios, but their
retired behavior is not an acceptance criterion and they are not application templates.
