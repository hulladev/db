# SolidStart: server-loaded tasks with live updates

Use a SolidStart server function for the first read, then `@hulla/db-solid` for
the browser subscription. Complete the [shared task setup](../meta-frameworks.md#set-up-the-shared-database-example)
first. This recipe uses SolidStart 2, Solid 1.9 and Solid Router's `createAsync`.

## Set the public browser endpoint

Set `database.client` in `db.config.ts` before generating the application:

```ts
// Inside defineConfig({ ... })
database: {
  server: { url: process.env.DATABASE_URL! },
  client: { url: process.env.PUBLIC_DATABASE_SERVICE_URL!, credentials: 'include' },
},
```

Generation copies only these public client settings into browser bindings. Importing
callables requires no setup call and opens no network connection. Use a root-relative
URL such as `/db` when your application proxies the service on the same origin.
The URL is selected at generation time; regenerate when changing environments.

For authenticated views, call generated `setAuth` from the provider's browser-only
session listener. Never put a request's identity in a shared server module. Component
cleanup releases its subscriptions; it does not dispose the shared application runtime.

## Load a snapshot through a server function

SolidStart's [server data functions](https://docs.solidjs.com/solid-start/v2/building-your-application/data-fetching)
are ordinary application code. Alias the router's `query` to distinguish it from
the database declaration. Keep the database import inside the server function:

```ts
// src/data/tasks.ts
import { query as serverQuery } from '@solidjs/router'

export const getInitialTasks = serverQuery(async () => {
  'use server'
  const { db } = await import('../../db/_generated/server')
  const { readTasks } = await import('../../db/task-data')
  return await readTasks(db)
}, 'initial-tasks')
```

Resolve the snapshot through Solid Router and pass it into a separate live
component. The conditional creates the binding only when initial data exists:

```tsx
// src/routes/index.tsx
import { createAsync } from '@solidjs/router'
import { Show, Suspense } from 'solid-js'
import { getInitialTasks } from '../data/tasks'
import { Tasks } from '../components/Tasks'

export default function Page() {
  const initial = createAsync(() => getInitialTasks())
  return <Suspense fallback={<p>Loading tasks…</p>}>
    <Show when={initial()}>{rows => <Tasks initialTasks={rows()} />}</Show>
  </Suspense>
}
```

The binding is created during component initialization and observes after mount:

```tsx
// src/components/Tasks.tsx
import type { Output } from '@hulla/db'
import { createMutation, createQuery } from '@hulla/db-solid'
import { For, Show } from 'solid-js'
import { create, list } from '../../db/_generated/client/tasks'

export function Tasks(props: { initialTasks: Output<typeof list> }) {
  const tasks = createQuery(list, { initialData: props.initialTasks })
  const add = createMutation(create)
  return <>
    <Show when={tasks().status === 'error'}><p>Could not load tasks.</p></Show>
    <Show when={add.state().status === 'error'}><p>Could not save the task.</p></Show>
    <ul><For each={tasks().data}>{task => <li>{task.title}</li>}</For></ul>
    <button onClick={() => { void add({ title: 'New task' }) }}>
      Add task
    </button>
  </>
}
```

Read query state through `tasks()` and mutation state through `add.state()`.
Queries with input can read signals through a getter. The router resource handles
the initial server loading boundary; database snapshots handle subsequent live
updates. Add a normal Solid error boundary for rejected server loads.

## Keep authorization in the server function

Obtain the request with `getRequestEvent()` from `solid-js/web` inside the server
function, reject a missing request/session, and verify its `request.headers`
through your provider before direct reads. Return the verified account key with
the rows and pass it as `initialAuthKey` to the binding. The browser declarations
need their own authorization and filters. Do not put private snapshots into a
shared application cache. See the [shared auth guide](../meta-frameworks.md#keep-server-data-private-when-adding-authentication).

Use other SolidStart server functions/actions for external effects and direct
`db.transaction` work. Router query invalidation affects router-loaded snapshots;
the database subscription independently observes committed writes. Neither
invalidates the other's cache automatically.

Choose a Node-compatible deployment target and include the native executables.
The persistent subscription service remains separate from the request-time app;
see [deployment and packaging](../deployment.md).
