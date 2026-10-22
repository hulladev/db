# TypeScript handler execution

Queries and mutations run in a fresh restricted JavaScript process. Native code owns
its PostgreSQL snapshot or transaction, constraints, concurrency checks and commit.
Use [the service setup](browser-service.md) to generate and serve declarations;
applications do not maintain a worker entrypoint or function registration list.
Ordinary server database calls run in your application's own runtime.

## Declare functions

```ts
// db/tasks.ts
import { z } from 'zod'
import { query, mutation } from './_generated/functions'

export const list = query(ctx =>
  ctx.db.select('tasks').where({ completed: false })
)

export const create = mutation({
  input: z.object({ title: z.string().min(1) }),
  handler: (ctx, input) => ctx.db.insert('tasks', input),
})
```

Use the callback shorthand when no options are needed. Otherwise use one object
containing `input`, `handler` and optionally `output`. Input and output accept
Standard Schema v1 validators. Your application chooses its validation library;
database fields use `v` from `@hulla/db/schema`.

Extensions execute in declaration order. Each input validator receives the original
submitted value; its parsed fields join the second handler argument. An extension's
returned properties join `ctx`. Parsed input keys cannot overlap across stages,
and context additions cannot replace existing or reserved properties such as `db`
or `identity`. A failure stops subsequent stages. Validators in a chain must accept
the shared payload and return their own fields; a strict validator that rejects
another stage's fields will reject that invocation. A single input validator can
return a scalar or array; several input validators require object results.

Validation can transform values and await computation. The SDK does not implicitly
coerce input. Browser argument types describe submitted values; handler input types
describe parsed values. Output validation runs before commit, and its transformed
result becomes the public return value. Output validation failure rolls back writes.

`ctx.identity` is the verified identity or `null`, inferred from your config's auth
callback. Enforce authorization in the handler or an extension. The auth callback
runs in the application process and can use your provider's normal verification
methods. Its implementation and credentials are not bundled into handlers.

## Supported handler code

Generation bundles declarations and imported helpers into one private script.
Execution uses QuickJS-NG through the pinned Rust `rquickjs` dependency. Each
invocation, including each OCC retry, starts with fresh module state.

Handlers support ordinary JavaScript computation, promises, regular expressions,
and bundled validation code. They cannot use network requests, files, process
APIs, native modules, timers, dynamic imports or dynamic code compilation. The
generator rejects statically visible unsupported imports before evaluating selected
declarations. Runtime restrictions also apply to imported helpers and validators.
A package being usable in Bun or Node does not establish handler compatibility.

Ambient time and randomness are unavailable. Explicit timestamp or timezone-bearing
Date construction and UTC operations are supported; `Date.now()`, `new Date()`,
local-time methods and `Math.random()` throw. Fetching external data or sending
email belongs in an ordinary application server function or API route.

Configuration, schema loading and generation are trusted build-time operations;
they are not sandboxed. Keep declaration modules free of top-level side effects.
Direct server code and the auth verifier retain their normal application capabilities.

The generated `_generated/worker.ts` and bundled `worker.js` are implementation
artifacts. Browser bindings contain references and type-only imports, not handler
code. There is no alternate Bun handler runtime or per-function runtime selector.

## Transaction boundaries

Database operations use the same awaitable builders as [direct server code](server-database.md).
This includes scalar comparisons, grouped callbacks, generated table references
and deterministic `orderBy`; see [filtering and ordering](server-database.md#filter-and-order-rows).
A query exposes only reads; native code also rejects write attempts. A mutation
runs in one serializable transaction with bounded retries after confirmed conflict
rollback. External effects cannot join this transaction.

Await every started database operation before returning. Concurrent calls serialize
inside the transaction. A database error ends the invocation; catching it cannot
resume that transaction. Result serialization cannot start further database work.
Unfinished requests, detached async failures, invalid protocol messages, a crash,
a timeout or failed validation prevent commit. Cancellation kills the worker and
rolls back before commit. A connection failure after commit remains a separate
mutation-confirmation concern; it is not evidence that the write rolled back.

The runtime has no inherited credentials or environment access. The native host
verifies operation sequences and waits for successful worker exit before accepting
its result. Errors cross this boundary as sanitized codes rather than raw stacks
or dependency errors. Engine isolation is not a claim of an independent security
audit.

## Current development bounds

Each invocation defaults to five seconds, a 64 MiB JavaScript heap, a 512 KiB
stack and 1,000 database operations. Input and output each default to 16 MiB.
Use [config limits and final-declaration overrides](execution-limits.md) to tune
them; generated framing accommodates raised payloads. Native buffers are separate
from the worker heap, and fetched rows/bytes have their own streaming budgets.
The native supervisor kills workers that exceed its deadline, including work that
does not yield to the JavaScript interrupt. Process startup and compilation happen
per invocation; production performance has not been benchmarked.

[JSON/array fields and filters](json-values.md) are supported. [Field selections](server-database.md#return-only-the-fields-you-need) narrow reads
and write results. [Aggregates and grouping](server-database.md#summarize-rows) return
typed summaries and participate in live refreshes. Function bigint results preserve
values beyond int64, while `v.int64()` columns still enforce their storage range.
Built-in Date results preserve their type; [field codecs](field-codecs.md) define
application/storage conversions. Ordinary observed queries already
use coordinated publication, and browser mutations use durable result confirmation;
see [browser service setup](browser-service.md). Native distribution requirements
are covered in [deployment](deployment.md).

## Validate the integration

```sh
make typescript-functions-test
make browser-service-test
```

These targets require Bun and PostgreSQL and build the native handler engine. They
exercise generated code, database writes and rollback, schema matching, cancellation,
protocol bounds and live subscriptions. The native engine's capability/deadline tests
can run without PostgreSQL:

```sh
cargo test -p hulladb_js --locked
```
