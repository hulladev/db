# SvelteKit: server-loaded tasks with live updates

Use a server `load` function for the first rows and `@hulla/db-svelte` inside the
page's component for live results. Complete the [shared task setup](../meta-frameworks.md#set-up-the-shared-database-example)
first. This recipe uses SvelteKit's server loaders/client init hook and Svelte 5
runes, with `db/` at the project root.

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

## Load rows on the server

A `+page.server.ts` module stays on the server. A universal `+page.ts` can also run
in the browser and must not import the generated server database:

```ts
// src/routes/+page.server.ts
import { db } from '../../db/_generated/server'
import { readTasks } from '../../db/task-data'

export const load = async () => ({ initialTasks: await readTasks(db) })
```

Forward the [loaded page data](https://svelte.dev/docs/kit/load) to a component:

```svelte
<!-- src/routes/+page.svelte -->
<script lang="ts">
  import Tasks from '$lib/Tasks.svelte'
  import type { PageData } from './$types'
  let { data }: { data: PageData } = $props()
</script>

<Tasks initialTasks={data.initialTasks} />
```

The live component displays those rows during SSR and subscribes after mount:

```svelte
<!-- src/lib/Tasks.svelte -->
<script lang="ts">
  import type { Output } from '@hulla/db'
  import { createMutation, createQuery } from '@hulla/db-svelte'
  import { create, list } from '../../db/_generated/client/tasks'

  let { initialTasks }: { initialTasks: Output<typeof list> } = $props()
  const tasks = createQuery(list, { initialData: initialTasks })
  const add = createMutation(create)
  const addTask = () => { void add({ title: 'New task' }) }
</script>

{#if tasks.status === 'error'}<p>Could not load tasks.</p>{/if}
{#if add.state.status === 'error'}<p>Could not save the task.</p>{/if}
<ul>{#each tasks.data ?? [] as task (task.id)}<li>{task.title}</li>{/each}</ul>
<button onclick={addTask}>Add task</button>
```

Read `tasks.data`, `tasks.status` and `add.state` reactively. Destructuring the
getters into ordinary variables loses updates. Queries with input accept a
getter, such as `() => ({ completed: filter })`, to read rune state.

## Use server actions for application work

SvelteKit form actions and server endpoints can use direct `db` operations.
Authenticate `event.request.headers`, enforce ownership and group writes with
`db.transaction` when rollback must apply to the whole operation. Server-loader
reruns and SvelteKit invalidation are separate from the live query's updates.

For protected SSR data, return a verified account key with the rows and pass it
as `initialAuthKey` to `createQuery`; update the same key from the browser auth
listener. Guard query/mutation declarations independently. Disable prerendering
and shared response caching for private request-time pages. See the
[shared authentication boundary](../meta-frameworks.md#keep-server-data-private-when-adding-authentication).

Deploy using a compatible Node/Bun adapter and include the database package's
native assets. Browser-only/static deployments can still use live declarations,
but cannot execute these direct server loaders. See [deployment](../deployment.md).
