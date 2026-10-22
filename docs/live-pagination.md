# Stable live pagination

Use pagination when a live result is too large to load at once. Each loaded page
keeps its original ordered range. Inserts and deletes can make that range grow or
shrink; the SDK does not refill it by borrowing rows from the next page.

## Return a page from a query

This example assumes a `tasks` table with a string `title`. Limit and cursor are
ordinary validated input, so the function controls its own pagination API:

```ts
// db/tasks.ts
import { z } from 'zod'
import { query } from './_generated/functions'

export const list = query({
  input: z.object({ limit: z.number().int().min(1).max(100), cursor: z.string().nullable() }),
  handler: (ctx, input) => ctx.db.select('tasks')
    .orderBy('title')
    .paginate(input),
})
```

Add authorization checks and filters before `paginate` for protected rows. Return
the database page directly. Generation recognizes its `Page<Row>` type and exposes
the client pagination helper. Reconstructing the result as a plain object or
transforming it into another output type does not make that query eligible.
Mutations cannot declare a live paginated result.

The page is `{ page: rows, cursor: string | null }`. A null cursor means this is
the last range. Cursors are opaque: pass them back unchanged. They bind to database
source, scope, schema, filters and ordering. Changing those conditions requires a
new first page. Cursors never authorize access; the handler checks the current
caller on every evaluation.

## Observe a growing list

React, Vue, Svelte and Solid applications can use the [paginated framework
bindings](framework-bindings.md#load-more-pages-in-a-component) to own this
lifecycle automatically. The plain JavaScript API below uses the same core.

After configuring the generated browser runtime:

```ts
import { list } from './db/_generated/client/tasks'

const tasks = list.paginate(cursor => ({ limit: 20, cursor }))
const stop = tasks.subscribe(snapshot => {
  if (snapshot.data) renderTasks(snapshot.data)
  if (snapshot.status === 'error') renderError(snapshot.error)
})

await tasks // First page; loading already started when the handle was created.
await tasks.next() // Append one page.
```

`snapshot.data` is one readonly array of all loaded rows. `tasks.hasMore` reports
whether a continuation exists. `next(3)` appends up to three pages sequentially.
Overlapping calls share the current load; the first determines its page count.
Once exhausted, `next()` does nothing.

Appending uses `status: 'pending'` and retains existing data. There is no separate
loading-more state. A failed append preserves successful pages; the next call
retries from the last successful page. Existing pages keep refreshing while
another page loads.

Existing `withOptimistic` query targets also reach matching loaded pages, including
separate lists with the same input. Their updater receives the query's `Page<Row>`
result; the list flattens the updated rows. Predictions cannot change the confirmed
continuation cursor or range boundaries. The updater still decides which rows to
change; the SDK does not infer a new row's position in a filtered or sorted list.

Forward the supplied cursor in the mapper; repeating an already-loaded input or
returning a non-advancing continuation fails instead of appending the same page.
Keep non-cursor input stable for the handle. Changing filters or sorting means
disposing it and creating a new one. The mapper is ordinary code; keep it free of
side effects. It does not make captured variables reactive.

Call `stop()` on view removal. Removing the last observer disposes the list and all
its page subscriptions. Without a subscriber, call `tasks.dispose()` explicitly
when finished. Auth changes retire the entire list; create a new handle under the
new identity.

## How rows move between pages

Suppose page one ends at title `B`, and page two covers titles after `B` through
`D`. Inserting `AA` grows page one. Deleting `B` leaves the boundary at `B`;
page one does not pull `C` from page two. Changing `A` to `C` moves that row to
page two. Both pages update in one coordinated query round, so the list publishes
the move together.

Ordering defaults to primary key. Custom sorting appends a primary-key tiebreaker
in the last sort direction. Multiple sort keys and nullable values follow
PostgreSQL's default ordering: nulls last ascending, first descending. Boundaries
include those ordering values, so deleting the boundary row does not invalidate
its range. A range discovered as exhausted has no upper boundary; later inserts
into that range remain visible.

## Refresh errors and limits

A failed refresh affects the whole list. Data clears by default; a subscription
can set `{ keepPreviousDataOnError: true }` to retain its last complete result as
stale. Authorization and identity failures always clear protected data. Relevant
database changes retry failed evaluation and can restore the list automatically.
Interrupted connections retain the last complete view as stale.

Current bounds are 1,000 requested rows per page, 1,000 rows in a refreshed range,
10,000 loaded rows per list, 128 loaded pages, and a 16 MiB list budget. Pages also
share the runtime's 128-query live-set and frame budgets. The native range has a
separate physical-row byte budget; serialized results must fit worker and delivery
frames too. Cursor and range tokens are bounded to 64 KiB, including their ordering
values; avoid sorting pages by very large text fields. A growing range that exceeds a bound produces an explicit error.
It is never silently truncated, refilled or split. Configurable limits remain
separate implementation work.

## Use pages in server code

Direct server reads use the same builder without a live subscription:

```ts
import { db } from './db/_generated/server'

const first = await db.select('tasks').orderBy('title').paginate({ limit: 20 })
const second = first.cursor === null ? null : await db.select('tasks')
  .orderBy('title')
  .paginate({ limit: 20, cursor: first.cursor })
```

`paginate`, `limit` and `unique` are terminal alternatives. Their return types
prevent combining them, and the runtime exposes no further modifiers. Direct
operations remain lazy and memoized. Use a server transaction when several
one-time reads must share a database snapshot.

A selection such as `db.select('tasks', ['title']).orderBy('title')` also supports
pagination. Cursor boundaries retain the key and sort values internally; the
public page contains only selected properties. Keep the selection unchanged when
continuing a cursor. See [field selections](server-database.md#return-only-the-fields-you-need).
