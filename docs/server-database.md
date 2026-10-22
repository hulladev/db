# Direct server database access

The generated server entrypoint supports ordinary database calls and transactions.
Your application server owns authentication, authorization and external services.
Direct operations do not invoke query/mutation declarations or create recovery
receipts. They use the same PostgreSQL storage and revision counter as live queries.

The SDK supports scalar and JSON fields,
nested predicates, scalar ordering, pagination, limits, uniqueness checks and
inserts/updates/deletes, aggregates and grouping. See [browser service setup](browser-service.md) to serve
discovered queries/mutations alongside direct database access.

## Define the application

Keep connection settings separate from the schema:

```ts
// db.config.ts
import { defineConfig } from '@hulla/db/config'

export default defineConfig({
  database: { server: {
    url: process.env.DATABASE_URL!,
  } },
})
```

```ts
// db/schema.ts
import { defineSchema, defineTable, v } from '@hulla/db/schema'

export default defineSchema({
  tasks: defineTable({
    title: v.string(),
    completed: v.boolean().default(false),
  }),
})
```

An `id` is generated for each table by default. `schema` in `defineConfig` can point
to another schema file; `functions` can override the directory scanned for exported
query/mutation declarations. Both paths resolve relative to the config file.
`database.server.namespace` optionally isolates an application's storage within PostgreSQL;
it defaults to `hulla`. Application tables currently share one internal storage scope.

## Generate and initialize

Install the SDK in your application and generate from the directory containing
`db.config.ts` (generation is a build-time tool):

```sh
npm install @hulla/db@0.1.0
npm install --save-dev typescript@7.0.2
bun --bun hulla-db generate db.config.ts
bun --bun hulla-db init db.config.ts
```

The npm package includes compiled JavaScript, declarations and native executables
for macOS arm64 and Linux x64/glibc. It does not compile Rust or download executables
during installation. See [deployment](deployment.md) for supported hosts.
The generated database runs in Node 22.18+ or Bun; using Bun for generation does
not select the application runtime.

Generation produces `_generated/server`, `functions`, `types`, `tables` and `client` beside
the schema. No listener or database connection starts during generation or import.
Server connection settings and handler implementations are excluded from generated
browser bindings.

Initialization is explicit. Repeating it verifies matching storage and preserves
existing data. An incompatible schema produces an error; initialization never
silently resets it or applies an upgrade migration. Use an appropriate provisioning
credential for this command and application credentials for normal access.

## Read and write

```ts
import { db } from './db/_generated/server'

const task = await db.insert('tasks', { title: 'First task' })

const unfinished = await db.select('tasks')
  .where({ completed: false })
  .limit(20)

const found = await db.select('tasks')
  .where('id', '=', task.id)
  .unique()

const updated = await db.update('tasks', { completed: true })
  .where({ id: task.id })

const deleted = await db.delete('tasks').where({ id: task.id })
```

Returned values are plain rows with IDs. A single insert returns one row; bulk
inserts, selections, updates and deletes return arrays. `.unique()` returns a row
or `null`, and throws if more than one matches. `.limit(1)` still returns an array.
There is no implicit row limit on an unbounded select; use a limit when appropriate.
Use [stable pagination](live-pagination.md) to fetch a bounded page with an opaque
continuation cursor, or observe an accumulating list in browser code.
Repeated `.where(...)` calls combine with AND, including repeated properties.

Update and delete require `.where(...)` before they can execute.
`.where(true)` explicitly selects every row in the table. Unknown properties are
rejected. An undefined patch value leaves the field unchanged; `null` is allowed
only for nullable fields. No caller-supplied row version is needed.

Builders are lazy: constructing one does not execute it. Awaiting or calling
`.then`, `.catch` or `.finally` starts the operation; subsequent awaits reuse the
same result or error. Build branches before executing them. A root operation commits
independently from other root operations.

## Handle a specific insert conflict

Use `onConflict` when a repeated insert should update a known row or skip that
insert. Declare the uniqueness rule in the schema first. For example, this
`contacts` table gives each email one row:

```ts
contacts: defineTable({
  email: v.string(),
  name: v.string(),
  visits: v.int64({ mode: 'bigint' }).default(0n),
}, { unique: { email: { fields: ['email'] } } })
```

Then server code can insert a contact and update its name when the email already
exists. The result is the inserted or updated row:

```ts
const contact = await db.insert('contacts', {
  email: 'ada@example.com', name: 'Ada',
}).onConflict(({ on }) => on('email').update({ name: 'Ada' }))
```

`on` accepts one complete primary or declared unique key. For a composite rule,
pass every field, such as `on('organizationId', 'email')`. Target suggestions use
declared field tuples; an incomplete key is an error. Runtime validation also
checks the real schema. Each insert has one conflict policy. Other uniqueness,
reference, validation and database constraints still apply.

Use `.skip()` instead of `.update(...)` to leave the existing row unchanged:

```ts
const created = await db.insert('contacts', {
  email: 'ada@example.com', name: 'Ada',
}).onConflict(({ on }) => on('email').skip()).returning(['id'])
// { id } for an insert, null for a skip.
```

Bulk inserts return only inserted or updated rows, omitting skipped entries.
Do not match bulk results to inputs by array position; result order is not
guaranteed. A bulk conflict update that would update the same existing row twice
fails, following PostgreSQL's single-statement cardinality rule.

## Update from the current or incoming values

A conflict update callback can copy an incoming value or add to the existing
value without a separate read. These references build database expressions:

```ts
await db.insert('contacts', {
  email: 'ada@example.com', name: 'Ada', visits: 1n,
}).onConflict(({ on }) => on('email').update(({ current, incoming }) => ({
  name: incoming.name,
  visits: current.visits.add(incoming.visits),
})))
```

`current` refers to the existing row and `incoming` to the proposed insert. In a
bulk insert each incoming reference uses its own row. PostgreSQL evaluates the
assignment atomically. A normal update callback uses the same expressions:

```ts
await db.update('contacts', current => ({ visits: current.visits.add(1n) }))
  .where('email', '=', 'ada@example.com')
```

Whole-column references require compatible types. Numeric `add` requires the same
numeric representation on both sides: bigint for bigint-mode int64, numbers for
number-mode int64 or number columns.
Nullable operands preserve SQL null propagation. Overflow and non-finite results
fail the write. These callbacks are expression declarations; JavaScript arithmetic
does not translate to SQL. Other arithmetic operators and partial JSON updates
are not currently supported. Primary keys cannot be changed.

## Require an affected-row count

Ordinary writes may affect zero rows. Add `expectRows` when that must be an error,
for example when a route must update exactly one existing contact:

```ts
await db.update('contacts', { name: 'Ada Lovelace' })
  .where('email', '=', 'ada@example.com')
  .expectRows(1)

await db.delete('contacts').where('name', '=', 'Unused')
  .expectRows(count => count >= 1 && count <= 10)
```

The exact-count form accepts a nonnegative safe integer. The predicate must return
a synchronous boolean. A mismatch throws `DatabaseError` with code
`row_expectation_failed`; a thrown exception or invalid predicate result also
fails the operation. This check runs before commit. Catching the error cannot
make its transaction committable again, including earlier writes in that transaction.

The same helper works on single/bulk inserts, conflict policies, updates and deletes,
through `db`, `tx` and writable `ctx.db`. An update counts a matched row even when
its values stay equal. Skipped inserts count zero; bulk operations check the total
affected count. `returning` changes fields, not that count. An expectation never
limits or truncates a write. Attach one expectation per operation, before execution.
Keep predicates free of side effects: retried mutations can evaluate them again.

## Return only the fields you need

Pass a selection as the second argument to `select`. Filters and ordering can
still use unselected fields, so the result can omit details used to find the rows.

```ts
const summaries = await db.select('tasks', ['id', 'title'])
  .where('completed', '=', false)
  .orderBy('title')
```

An inclusion map provides the same selection. Use runtime booleans for optional
fields; generated function results preserve that optionality for browser callers.

```ts
function listTasks(includeCompleted: boolean) {
  return db.select('tasks', { title: true, completed: includeCompleted })
}
// Result: { title: string; completed?: boolean }[]
```

Arrays also accept conditions such as `['title', includeCompleted && 'completed']`.
Only `false`, `null` and `undefined` skip an array entry. In object selections,
`true` includes a field and those same three values exclude it. Literal inclusions
are required result properties; runtime inclusions are optional properties.
A selected nullable field keeps its nullability. Excluded properties are absent.

Selections are captured when the builder is created. Unknown names, invalid
selection values and an empty effective selection throw before execution. Repeated
array names select that field once. Select complete root columns; nested JSON
selection, aliases and computed expressions are not supported by this API.
A projection does not authorize access: keep authorization checks in your handler.

Writes return full rows by default. Add `returning` to narrow their result without
changing what gets written or which constraints apply:

```ts
const created = await db.insert('tasks', { title: 'First task' })
  .returning(['id'])
const changed = await db.update('tasks', { completed: true })
  .where('id', '=', created.id)
  .returning({ title: true, completed: true })
```

A single insert still returns one object; bulk inserts, updates and deletes return
arrays. `delete(...).where(...).returning(...)` uses the same selections. An empty
bulk insert returns `[]` without a write. All input validation and database
constraints still apply, including to fields omitted from the result.

`unique`, `limit` and `paginate` preserve the selected result shape. Pagination
retains ordering fields and key boundaries internally, even when those fields
are omitted from the public rows. Keep the same selection when continuing a cursor.
Configure all modifiers before awaiting a builder; changing it after execution
starts throws, and repeated awaits reuse its original result.

## Filter and order rows

Use an object for equality, or pass a field, operator and value. Field names are
relative to the selected table. Strings in the value position are always literal
values; use a generated field reference to compare two columns.

```ts
import { tasks } from './db/_generated/tables'

const matches = await db.select(tasks)
  .where('completed', '=', false)
  .where(tasks.title, 'contains', 'guide', { caseSensitive: false })
  .orderBy(tasks.title)
  .limit(20)
```

Table names and generated references can be mixed. References also work in inserts,
updates and deletes. They come from generated `tables`, not the authored
`defineTable` declarations. For a table name that is not a JavaScript export name,
use the default map: `import tables from './db/_generated/tables'`, then
`db.select(tables['Order History'])`.

For grouped conditions, destructure comparison helpers in a callback. Repeated
`.where(...)` calls AND their complete conditions, so parentheses remain explicit:

```ts
const candidates = await db.select(tasks)
  .where(({ or, startsWith }) => or(
    startsWith(tasks.title, 'Write'),
    startsWith(tasks.title, 'Review'),
  ))
  .where({ completed: false })
  .orderBy('title', 'desc')
```

Use `and`, `or` and `not` to compose comparisons. Empty `and()` matches every row;
empty `or()` matches none. The same predicates work on updates and deletes.

| Match | Positional operators | Callback helpers |
| --- | --- | --- |
| Equality | `=`, `!=` | `eq`, `ne` |
| Ordered comparison | `>`, `>=`, `<`, `<=` | `gt`, `gte`, `lt`, `lte` |
| Membership | `in`, `not in` | `inArray`, `notInArray` |
| Inclusive range | `between`, `not between` | `between`, wrapped in `not` for negation |
| Null | `is`, `is not` with `null` | `isNull`, `isNotNull` |
| Literal text | `contains`, `startsWith`, `endsWith` | Same names |
| SQL pattern | `like`, `ilike` | Same names |

Literal text matching treats `%`, `_` and backslashes as ordinary characters. It
is case sensitive unless `{ caseSensitive: false }` is supplied. `like` and
`ilike` use SQL patterns; `ilike` is case insensitive. Positional text operators
also accept a `not ` prefix; callbacks use `not(contains(...))`, for example.
Boolean fields support equality and membership, but not ordered comparisons.

`= null` and `!= null` become `IS NULL` and `IS NOT NULL`, including object filters.
Other comparisons follow SQL null semantics: `not(eq('excerpt', 'draft'))` does
not match null excerpts. Include `isNull('excerpt')` with `or` when that is wanted.
An `in` list containing `null` explicitly matches nulls; `not in` negates that
complete match. Empty membership lists match none (`in`) or all (`not in`).
Range bounds are inclusive and never reordered. Undefined filter values are errors.

Selections order by primary key by default. Chained `orderBy` calls add sort keys
in order; if the primary key is absent, it is appended in the last sort direction
to break ties. Null ordering follows PostgreSQL: last ascending, first descending.
This makes each result deterministic; it does not freeze rows across later reads.
Ordering does not create an index automatically.

Predicates currently allow at most 1,024 nodes, nesting through depth 32, 1,024
values per membership list and 4,095 bound predicate values. A selection accepts
up to 64 explicit sort keys. Oversized expressions
fail before SQL execution. See [JSON values and predicates](json-values.md) for
nested paths and array matching. Custom SQL expressions are not implemented yet.

## Summarize rows

Use `aggregate` when the caller needs a count or summary instead of individual
rows. For an `orders` table with `status: v.string()` and
`amount: v.number().nullable()`, this server call returns one object:

```ts
const summary = await db.select('orders')
  .where('status', '=', 'paid')
  .aggregate(a => ({
    count: a.count(),
    total: a.sum('amount'),
    average: a.avg('amount'),
    smallest: a.min('amount'),
    largest: a.max('amount'),
  }))
```

`count()` counts matching rows, including rows with null amounts. Other aggregates
ignore nulls. With no matching rows, `count` is `0` and the other results are `null`;
they also return `null` when every matching amount is null.

| Column | `sum` | `avg` | `min` / `max` |
| --- | --- | --- | --- |
| `v.number()` | `number \| null` | `number \| null` | `number \| null` |
| `v.int64()` | safe `number \| null` | `number \| null` | safe `number \| null` |
| `v.int64({ mode: 'bigint' })` | `bigint \| null` | decimal `string \| null` | `bigint \| null` |
| `v.string()` | Unsupported | Unsupported | `string \| null` |

Bigint sums can exceed the range of one stored int64 without losing precision.
Bigint averages preserve PostgreSQL's numeric result as a decimal string; they do
not round through a JavaScript number. Counts outside JavaScript's safe integer
range and non-finite numeric results throw. Boolean, UUID and JSON extrema are not
supported. Aggregate operands accept root column names or generated field references.

## Group and filter summaries

Add `groupBy` before `aggregate` to return an array, with grouping columns included
beside the named results. This finds paid-order summaries by owner, then keeps the
owners whose total exceeds 100:

```ts
const owners = await db.select('orders')
  .where('status', '=', 'paid')
  .groupBy('owner')
  .aggregate(a => ({ count: a.count(), total: a.sum('amount') }))
  .having(({ gt }) => gt('total', 100))
  .orderBy('total', 'desc')
  .limit(20)
// Array of { owner, count, total }; [] when no groups match.
```

`where` filters source rows; `having` filters completed groups. Its callback uses
grouping names and aggregate aliases with the same comparison helpers as row
filters. Decimal average aliases compare numerically: `gt('average', '9.5')`
does not use text ordering. Repeated `having` callbacks combine with AND.

Group keys must be distinct scalar root column names. Aggregate aliases cannot
reuse a group key. Ordering accepts group keys and aliases, appending any missing
group keys to break ties; with no explicit order it uses the grouping keys ascending.
`limit` bounds returned groups, not the input work required to compute them.

Configure row filters before grouping, and group ordering after aggregation.
Combining an aggregate with a row projection or earlier row ordering throws.
Grouped pagination is outside v1. These builders also work with `tx` in a
transaction and `ctx.db` inside queries/mutations. A live query returning an
aggregate refreshes when its source table changes.

## Group changes in a transaction

```ts
const tasks = await db.transaction(async tx => {
  const first = await tx.insert('tasks', { title: 'Write the guide' })
  const second = await tx.insert('tasks', { title: 'Review the guide' })
  return [first, second]
})
```

The callback runs once by default. An uncaught exception rolls back all its writes.
Catching a database failure cannot make the failed transaction committable again.
Ordinary caught application errors do not automatically undo earlier successful
statements. There are no implicit nested transactions or savepoints.

Use `tx` inside the callback. Calling the root `db` there fails before execution;
unrelated requests remain able to use it. A transaction handle cannot be used from
another request or after its callback completes. Await all started database work
before returning. An unused lazy builder performs no work.

Use `{ retries: 3 }` as the optional second argument to opt into up to three retries
after confirmed serialization rollback. The whole callback may run again. Constraint
violations, ordinary exceptions and uncertain commits are not retried. Keep external
effects outside retryable callbacks.

A lost or malformed commit acknowledgment produces `DatabaseError` with code
`commit_outcome_unknown`: the transaction may have committed. Do not treat that as
proof of rollback or replay the operation blindly. Direct server operations do not
have the browser mutation receipt-confirmation lifecycle.

The current engine uses a small lazy connection pool with a private local process
protocol. It does not open a network endpoint for direct database access. Call
`await db.close()` during application shutdown; an HTTP request finishing is not
application shutdown. Up to four native sessions are reused per database handle. Unused sessions drain
after ten seconds; reuse checks the connection before starting application work.
Outside a transaction, the bridge also retires after thirty seconds without a
command. A transaction callback instead uses its configured execution deadline
(five seconds by default), including time spent waiting between statements.
PostgreSQL enforces configured transaction execution/idle bounds, including when
the application is suspended. Keep network calls and other long waits outside the
transaction. Resuming an expired transaction cannot commit or automatically replay it.
See [configurable execution limits](execution-limits.md) for defaults, overrides and
fetched-data accounting, and [deployment](deployment.md)
for host requirements and the distinction between local tests and hosted support.

## Share ordinary helpers

```ts
import type { ReadDatabase } from './db/_generated/types'

export function unfinishedTasks(db: ReadDatabase) {
  return db.select('tasks').where({ completed: false })
}
```

A query can call this helper with `ctx.db`; server code can call it with `db` or `tx`.
Use `WriteDatabase` for helpers that write. `Database` includes transaction management
and application shutdown. Authentication checks belong in the caller's boundary;
passing a database handle does not authenticate a user.

## Validation

```sh
cargo test -p hulladb_server --features typescript --test direct_bridge --locked -- --ignored
```

Set `HULLADB_TEST_URL` to a disposable PostgreSQL database. These tests create and
clean up unique namespaces. They cover real generated setup/initialization, plain
row builders, rollback, scope ownership, concurrent requests, shared revisions,
large selections and serialization conflicts. SDK tests separately inject invalid
commit acknowledgments to verify uncertainty is preserved without replay.

## Resource limits and shutdown

Each database owns up to four engine sessions. At most 64 callers can wait for a
session within the configured execution period; excess callers receive `capacity`.
A transaction defaults to a five-second execution budget, including callback waits,
and at most 64 queued statements. Keep unrelated external work outside the
transaction. A deadline rejects with `transaction_timeout`; if a commit has already
been sent, lost confirmation still reports `commit_outcome_unknown`.

`close()` rejects waiting and active calls, fences retained transaction handles,
and waits for its engine processes to exit. An engine that ignores termination is
killed after two seconds. JavaScript callbacks themselves cannot be forcibly
cancelled; their later database calls fail after the lease is retired.

For structured settings, nested paths, and exact array membership, see
[JSON values and predicates](json-values.md).
