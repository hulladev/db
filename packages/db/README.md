# TypeScript function SDK

`@hulla/db` provides direct PostgreSQL access and generated reactive
function calls. The generated server database supports lazy CRUD builders, typed
filters/projections, grouping and aggregates, conflicts, stable pagination and
scoped transactions in Node and Bun; see [direct server setup](../../docs/server-database.md).
The supplied service and generated browser setup are described in
[browser service setup](../../docs/browser-service.md). Handlers execute in the
[restricted native runtime](../../docs/typescript-functions.md#supported-handler-code).
Schema authoring includes scalar constraints, named SQL checks, explicit indexes,
references/deletion policies, field codecs and optional Zod validation adapters;
see [schema setup](../../docs/schema-authoring.md).

Separate `@hulla/db-react`, `@hulla/db-vue`, `@hulla/db-svelte` and
`@hulla/db-solid` packages bind generated callables to UI reactivity. They share
the core cache, support live pagination and authorized SSR `initialData`; see
[framework bindings](../../docs/framework-bindings.md) and the
[meta-framework recipes](../../docs/meta-frameworks.md). Core `Input<typeof fn>`
and `Output<typeof fn>` helpers infer declaration and callable contracts.
Optional `@hulla/db-better-auth` stores provider data in PostgreSQL; see the
[Better Auth setup](../../docs/better-auth.md).

Generation and the development watcher use Bun. Production `serve` loads prepared
artifacts without generation and can run under Node or Bun; see the setup guide for
source-checkout commands. Local installable artifacts contain compiled JavaScript,
declarations and native binaries; see [deployment and packaging](../../docs/deployment.md).
There are no required npm runtime dependencies.
Generation and `dev` use the optional TypeScript 7.0.2 peer; applications using
those tools install it as a dev dependency (`bun add --dev typescript@7.0.2`).
Production `serve`, direct database access and browser bindings do not load it.
See [dependency boundaries](../../docs/dependencies.md) for the native components.

## Declaring functions

After schema generation, import `query` and `mutation` from
`./_generated/functions`. Use `{ input, handler, output? }` with your own Standard
Schema validator, or pass a callback directly when no options are needed.
`.extend(...)` adds ordered input validation and context preparation. Output
validation and transformations finish before commit. See the
[authoring example](../../docs/typescript-functions.md#declare-functions).

## Calling and observing functions

Generated bindings contain contracts and invocation code, not server handlers:

```ts
import { pending, create } from './db/_generated/client/notes'

const save = create({ title: 'Ship it' }) // Starts immediately.
const stopSave = save.subscribe(snapshot => {
  renderSaveStatus(snapshot.status)
})
const created = await save // Authoritative server result.
stopSave()

const unsubscribe = pending.subscribe(snapshot => {
  if (snapshot.status === 'success') renderNotes(snapshot.data)
  if (snapshot.status === 'error') renderError(snapshot.error)
})
// When the view is disposed:
unsubscribe()
```

Query and mutation handles start on invocation and expose `status`, `data`, `error`
and `subscribe`. Repeated awaits reuse the same execution. An awaited query is a
one-time read; observing keeps results live. `query.subscribe(...)` starts the live
read directly, without an additional one-shot request. Attaching to an already
started query handle also attaches live observation. Query data is recursively
readonly; copy values before editing them.

Actively observed queries in one HTTP runtime receive complete updates from one
database snapshot. All query handles update before application listeners run. A
one-shot response resolves its await without replacing an observed result outside
that batch. Unsubscribing removes the query from the live set; changing membership
discards in-flight updates for the old set. A slow query can delay its siblings.
Separate runtimes and tabs are independent.

Connection failures keep the last complete view as stale. Handler errors affect
only their query, which stays subscribed and can recover after a database change.
The current live set is bounded to 128 distinct queries, including loaded pages.
[Stable live pagination](../../docs/live-pagination.md) keeps page boundaries fixed
as rows change. Inactive results stay in memory for at most five
minutes, bounded to 128 entries and 16 MiB of encoded keys/results; oldest inactive
entries leave first. Active queries remain pinned. Re-observing always verifies a
fresh server round. Concurrent one-shot calls are capped at 128, with pending background receipt ACKs
counted against admission as well.

Query states are `pending`, `success` and `error`, with `isStale` and `isOptimistic`
indicators. Mutations additionally use `recovering` when confirmation is interrupted.
Query subscription options support `keepPreviousDataOnError`; auth/source retirement
always clears prior data. Query cancellation uses an AbortSignal. Mutations accept
only their input, with no cancellation or per-call mode options.

## Optimistic query updates

Wrap a mutation once and export that binding where prediction is useful:

```ts
export const optimisticCreate = create.withOptimistic({
  query: pending,
  update: (notes, input) => [
    ...notes,
    { id: -1, title: input.title, owner: 'alice' },
  ],
})

const saved = await optimisticCreate({ title: 'Ship it' })
```

This example uses the notes fixture's result shape. Application updates must match
their query result and use stable provisional identifiers appropriate to that model.
Capture any generated values outside the updater: updaters must be pure and may run
again. `saved` is the server result, never the provisional row.

Pass additional target objects as separate arguments to update several queries.
Each object returns a replacement query result. No-input queries need no `input`
mapper. For a query with input, supply an `input(mutationInput)` mapper for one
exact input or `match(queryInput, mutationInput)` to select already-cached variants.
A matching target does not require an input mapper. Missing cached results are
skipped permanently for that invocation. An updater failure removes that invocation's predictions without
preventing server execution.

Layers replay in submission order over the latest server base. A failure removes
only its layer; successful predictions remain until query results incorporate the
commit. Interrupted confirmation removes predictions immediately. Recovery does not
reapply them. There is no prediction configuration map, mode switch or dependency
queue; use the base callable when no optimism is wanted.

## Confirmation and errors

The HTTP runtime submits each mutation once. If confirmation is interrupted, the
same handle enters `recovering` and looks up the original receipt. A missing receipt
never causes resubmission. Reconnect attempts share one overall confirmation window,
30 seconds by default, configurable through `database.client.confirmationTimeoutMs`. Checks stop when the handle settles or its auth scope retires.

`DatabaseError({ code, message, details? })` is the public error class. Intentional
handler errors preserve these fields across calls, live-query delivery and mutation
failure recovery. The serialized payload is limited to 16 KiB; larger errors fail
with `execution_limit`. Stack traces, causes, ordinary exceptions and database
diagnostics are never included. Application codes do not control transport retries
or cache identity. Only declare information safe for the caller. A
`confirmation_timeout` or `confirmation_interrupted` does not establish rollback:
the server may have committed. `receipt_expired` and `invalid_result` preserve
`details.committed: true` when the receipt establishes a commit but its result is
unavailable or invalid. Never blindly resubmit based on a confirmation error.

Results, active handles and optimistic layers stay in memory. There is no browser
persistence, mutation journal, manual `recoverPending`, or reload recovery. A fresh
page reads the server. Closing the page does not cancel a server transaction.

## Browser setup and authentication

Generate the application with `hulla-db generate` (or use `hulla-db dev` while
working). Set `database.server` to the PostgreSQL connection and
`database.client` to public settings in `db.config.ts`:

```ts
database: {
  server: { url: process.env.DATABASE_URL! },
  client: { url: 'https://db.example.com', credentials: 'include' },
},
```

Generated callables initialize lazily; importing them opens no connection. Only
public settings are emitted, never server configuration or credentials. Root-relative
URLs such as `/db` resolve against the browser origin. Regenerate when changing
these build-time settings. Auth changes remain application-owned:

```ts
import { setAuth } from './db/_generated/client/runtime'

// In your existing auth provider's session-change listener:
setAuth(session ? { key: session.user.id } : null)
```

Here `session` comes from your auth provider. Applications do not construct an HTTP
runtime, bind declaration factories or register function contracts themselves.
Generated imports contain browser callables; handlers and server credentials stay
on the service. Direct application-server code imports `db` from
`./db/_generated/server` and owns its authorization checks.

Use the actual user/tenant key from your auth subscription. It partitions client
state; it is never proof of identity. The server verifies credentials and supplies
the verified context. Applications using bearer tokens can call generated `setHeaders` with a
callback that reads their auth provider's current token. Cross-origin cookie setups
use `credentials: 'include'` and an explicitly allowed server origin.

The auth provider owns session validity, refresh and revocation. `setAuth` accepts
`{ key, expiresAt? }`, `null`, or a promise resolving to either. No status field is
needed: an object supplies the client identity, `null` clears it, and an unresolved
promise implies loading. `resolveAuth` above is application code using your chosen
provider; it returns this metadata, not credentials.

A promise immediately retires the previous scope. While it resolves, calls fail
with `authentication_pending` without sending requests. Use
`await setAuth(resolveAuth())` before calls that depend on that resolution. The
latest update wins, even if an older promise finishes later. If resolution rejects
or returns invalid metadata, the returned promise rejects and calls fail with
`authentication_failed` until another update; failure never silently grants anonymous
access. Reattach subscriptions once the new scope is ready.

The optional `expiresAt` is a Unix timestamp in milliseconds.
Omit it to rely on the application's existing auth notifications. When supplied,
its deadline clears local auth-scoped state just like `setAuth(null)`; a past deadline
clears it immediately. Updating only the deadline preserves a still-valid cache;
omitting it on a later update removes the timer. Browser timers can be delayed in
suspended tabs, so handle access and response delivery also check the deadline.
This does not refresh or revoke credentials, and server verification still applies.
Changing the key or calling `setAuth(null)` retires old calls, subscriptions, cached
results and optimistic updates immediately, including completed handles. Reattach
subscriptions for the new identity. Repeating the same synchronous identity does not clear results.
A `context_changed` error does not establish that an in-flight write rolled back.

When live credentials expire, the adapter can reconnect using credentials already
refreshed by the provider only if the server verifies the same context. It never
refreshes credentials itself or silently adopts a different identity. Invalid
credentials clear protected results even if the application missed an auth update.

For ordinary application setup, use `database.client` and `hulla-db
serve`; [the service guide](../../docs/browser-service.md) covers config, discovery
and provider verification. The explicit adapter construction above remains useful
for transport-level integration. Server request adapters must not share mutable
identity across requests.

The generator supports closed records, arrays, required tuples, literals/unions and
lossless bigint values and built-in Dates. Unsupported shapes fail generation. Semantic validation
and authorization still run on the server. Schema authoring and current callback
registration are described in [schema authoring](../../docs/schema-authoring.md)
and [TypeScript execution](../../docs/typescript-functions.md).

Use [field representations and codecs](../../docs/field-codecs.md) for integer
modes, dates and explicit application-to-storage conversions. Configure server
budgets in `defineConfig({ limits })` and override them on final declarations;
see [execution limits](../../docs/execution-limits.md) for defaults and browser
timeout configuration.

## Release artifacts

Prepared release archives include macOS arm64 and Linux x64/glibc native
executables. Browser-only use does not launch them. See the
[release workflow](../../docs/releasing.md) for all six npm packages, Rust crates,
license notices and archive checks.

## Validation

Run `bun run check` from this package for generation, typechecking and SDK tests.
Focused test files can run independently when the change is narrower.

The browser harness (`bun tests/browser-check-server.ts`) checks actual browser
confirmation, fresh reads after reload, int64 and identity cleanup. PostgreSQL-backed
HTTP/SSE and worker tests are separate and require a running database; see
[SDK adapter validation](../../docs/sdk-adapters.md#validation).
