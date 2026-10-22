# Framework bindings and server rendering

Install the core and the binding for your UI framework. These are separate
packages; the core has no React, Vue, Svelte or Solid dependency. Each binding
uses the application's existing framework and the same core runtime/cache.
The packages remain unpublished; use workspace or locally built artifacts.

| Package | Query result | Mutation state |
| --- | --- | --- |
| `@hulla/db-react` | `useQuery(fn, input)` → snapshot | `useMutation(fn)` → callable, `save.state` |
| `@hulla/db-vue` | `useQuery(fn, () => input)` → readonly ref | `useMutation(fn)` → callable, `save.state.value` |
| `@hulla/db-svelte` | `createQuery(fn, () => input)` → reactive getters | `createMutation(fn)` → callable, `save.state` |
| `@hulla/db-solid` | `createQuery(fn, () => input)` → accessor | `createMutation(fn)` → callable, `save.state()` |

Configure the generated browser runtime once before mounting the application,
using [the existing service setup](browser-service.md#connect-the-browser).
Pass generated callables to the binding, without invoking them during render.
Bindings subscribe after mount and unsubscribe on removal. Equivalent encoded
inputs preserve the observation; changed inputs replace it and fence late replies.
Query results retain the core `status`, `data`, `error`, `isStale` and
`isOptimistic` properties. Vue/Svelte/Solid input getters follow framework reactivity.
Read Svelte snapshot getters reactively rather than destructuring them into locals.

Each mutation call returns its original awaitable handle. The binding's state
starts as `idle`, then follows the most recently started invocation through the
core pending/recovering/success/error states. Overlapping writes keep independent
outcomes. Unmounting stops UI observation; it does not cancel a submitted write.
Optimistic mutation exports created with `.withOptimistic(...)` work unchanged.

## Load more pages in a component

Use a query that directly returns a database page, as shown in
[live pagination](live-pagination.md#return-a-page-from-a-query). Supply a cursor
mapper; the binding owns all loaded page subscriptions and combines their rows:

```tsx
import { usePaginatedQuery } from '@hulla/db-react'
import { list } from './db/_generated/client/tasks'

function Tasks() {
  const tasks = usePaginatedQuery(list, cursor => ({ limit: 20, cursor }))
  return <>
    {tasks.data?.map(task => <p key={task.id}>{task.title}</p>)}
    <button disabled={!tasks.hasMore || tasks.status === 'pending'}
      onClick={() => { void tasks.next().catch(reportError) }}>Load more</button>
  </>
}
```

`reportError` is the application's error reporter. A failed load also appears in
the snapshot. `next(3)` loads up to three pages. Pending appends retain loaded rows;
there is no separate loading-more status. Unmounting releases every page.

| Framework | Construction | Read rows / continuation | Load more |
| --- | --- | --- | --- |
| React | `usePaginatedQuery(fn, cursor => input)` | `tasks.data`, `tasks.hasMore` | `tasks.next()` |
| Vue | `usePaginatedQuery(fn, cursor => input)` | `tasks.value.data`, `tasks.value.hasMore` | `tasks.next()` |
| Svelte | `createPaginatedQuery(fn, cursor => input)` | `tasks.data`, `tasks.hasMore` | `tasks.next()` |
| Solid | `createPaginatedQuery(fn, cursor => input)` | `tasks().data`, `tasks().hasMore` | `tasks.next()` |

Read reactive filters inside the mapper in Vue, Svelte and Solid. In React,
capture the current rendered props/state. A changed first-page input resets the
list; equivalent input keeps its pages. Keep all other input stable across page
cursors. Saved `next` callbacks always target the current mounted list.

The optional third argument accepts the same `initialData`, `initialAuthKey` and
error-retention settings as ordinary queries. Pagination `initialData` is the
flattened row array, without an authoritative cursor. After mount, the first live
page replaces that fallback and establishes whether more data exists. Account
changes retire all old pages and the seed before attaching a new live list.

## Infer inputs and results

Use the same helpers for declarations, generated callables and optimistic wrappers:

```ts
import type { Input, Output } from '@hulla/db'
import { listTasks, createTask } from './db/_generated/client/tasks'

type ListInput = Input<typeof listTasks>
type Tasks = Output<typeof listTasks>
type CreateInput = Input<typeof createTask>
type CreatedTask = Output<typeof createTask>
```

`Input` describes submitted values before schema transformations; no-input
declarations infer `undefined`. `Output` describes resolved data after output
validation and codecs, not the invocation handle or subscription status. Query
outputs are recursively readonly. The helpers require no generated type aliases.

## Hand server results to a live view

Server routes, loaders and components use [direct `db` access](server-database.md).
They own authorization and can use their framework's normal loading/streaming APIs.
Direct reads return snapshots; browser queries establish live subscriptions.

For example, an authorized server read may pass its selected task rows to a client
component as `initialTasks`. The generated `listTasks` query must return the same
selected shape for the same input, with its own authorization checks. Share a
database helper if needed to keep the two read paths aligned.

```tsx
'use client'
import type { Output } from '@hulla/db'
import { useQuery } from '@hulla/db-react'
import { listTasks } from './db/_generated/client/tasks'

export function Tasks({ initialTasks }: {
  initialTasks: Output<typeof listTasks>
}) {
  const tasks = useQuery(listTasks, { completed: false }, {
    initialData: initialTasks,
  })
  if (tasks.status === 'error') return <p>Could not load tasks.</p>
  if (!tasks.data) return <p>Loading…</p>
  return <ul>{tasks.data.map(task => <li key={task.id}>{task.title}</li>)}</ul>
}
```

No-input queries accept options as the second argument. All four bindings accept
the same query options. Rendering never starts a request, including during SSR.
The server and first hydration render display the same initial data; after mount,
the first live result replaces it. The fallback is `pending` with `isStale: true`,
and never enters the shared authoritative cache or optimistic cache updates.
It ends at the first result/error, an input change or an auth scope change.
Returning to an earlier input/account cannot resurrect it. Changing `initialData`
alone does not restart React's subscription or replace a live result.

For protected data, also pass `initialAuthKey`, obtained by the application from
the server-verified session. It must match the key supplied to browser `setAuth`.
Omitting it means anonymous data; it will not seed a signed-in scope. Neither
this key nor the initial data supplies server credentials or grants access.
Identity changes clear the old view and reattach subscriptions automatically;
promised auth updates pause observation while identity resolves. Provider expiry
and server rejection still clear protected results. Do not share private SSR
responses across accounts in the application's routing/cache layer.

`keepPreviousDataOnError: true` retains previously successful live data on ordinary
query errors. It never retains an unconfirmed SSR fallback or data after an
authorization failure. The core owns query caching, optimistic layers and mutation
confirmation; the bindings do not add another cache, retry queue or auth provider.

Server-loaded props use the framework's serialization rules. Forward only the
selected, authorized result; use framework-supported serialization when working
with codecs or other non-JSON representations.

For full application setup, choose a [meta-framework recipe](meta-frameworks.md)
for Next.js, Nuxt, SvelteKit, SolidStart or Astro. The separate
[Better Auth adapter](better-auth.md) supplies provider storage and schema support.
