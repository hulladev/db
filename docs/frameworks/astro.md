# Astro: server-rendered tasks in a live island

Use an Astro page for a request-time database read and a hydrated React island
for live results. Complete the [shared task setup](../meta-frameworks.md#set-up-the-shared-database-example)
first. This recipe uses Astro's normal React integration and `@hulla/db-react`;
Vue, Svelte or Solid islands use their corresponding database binding instead.

## Enable request-time rendering and React

Use a compatible server adapter. For a Node standalone deployment:

```js
// astro.config.mjs
import { defineConfig } from 'astro/config'
import node from '@astrojs/node'
import react from '@astrojs/react'

export default defineConfig({
  output: 'server',
  adapter: node({ mode: 'standalone' }),
  integrations: [react()],
  vite: { ssr: { external: ['@hulla/db'] } },
})
```

Install the framework's `@astrojs/node` and `@astrojs/react` integrations alongside
React and the database packages. The [server-rendered page](https://docs.astro.build/en/guides/on-demand-rendering/)
runs its frontmatter per request; static prerendered pages instead capture data
at build time and must not contain a private user's result.

## Share the generated island runtime

Set the public endpoint and credential mode in `database.client` in `db.config.ts`
before generation. All islands importing generated callables share one lazy browser
runtime. Imports open no connection and require no configuration module.

For private views, call generated `setAuth` from your browser-only provider listener.
Keep its key synchronized when navigating between accounts. An island's unmount
releases its subscriptions; it must not dispose the shared runtime used by other islands.

## Pass the first rows into a hydrated island

Read the selected rows in frontmatter, then use `client:load` to enable browser
observation without losing the server-rendered HTML:

```astro
---
// src/pages/index.astro
import { db } from '../../db/_generated/server'
import { readTasks } from '../../db/task-data'
import { Tasks } from '../components/Tasks'

const initialTasks = await readTasks(db)
---
<html lang="en">
  <head><title>Live tasks</title></head>
  <body><Tasks initialTasks={initialTasks} client:load /></body>
</html>
```

The React component shares the same hooks as other React applications:

```tsx
// src/components/Tasks.tsx
import type { Output } from '@hulla/db'
import { useMutation, useQuery } from '@hulla/db-react'
import { create, list } from '../../db/_generated/client/tasks'

export function Tasks({ initialTasks }: { initialTasks: Output<typeof list> }) {
  const tasks = useQuery(list, { initialData: initialTasks })
  const add = useMutation(create)
  return <>
    {tasks.status === 'error' && <p>Could not load tasks.</p>}
    {add.state.status === 'error' && <p>Could not save the task.</p>}
    <ul>{tasks.data?.map(task => <li key={task.id}>{task.title}</li>)}</ul>
    <button onClick={() => { void add({ title: 'New task' }) }}>
      Add task
    </button>
  </>
}
```

Without a [client hydration directive](https://docs.astro.build/en/guides/framework-components/),
the island remains HTML and never subscribes. `client:only` skips its server render;
use `client:load` for this SSR handoff. Other hydration timings delay when live
observation begins. Multiple React islands can observe the same generated query
and share the database cache without sharing a React context.

## Protect request-time data

Verify `Astro.request.headers` through your provider before a private read, or use
a verified identity installed by application middleware in `Astro.locals`. Pass
the resulting account key as `initialAuthKey` to the binding along with its rows.
Do not prerender or publicly cache private pages. Query/mutation declarations
still need their own checks; see the [shared auth setup](../meta-frameworks.md#keep-server-data-private-when-adding-authentication).

Astro actions and API endpoints can use direct `db` operations and transactions;
their writes refresh live islands. The Astro page/frontmatter does not itself
become live. Keep native assets with the server output, use a host permitting
subprocesses, and deploy the persistent streaming service separately. See
[deployment requirements](../deployment.md).
