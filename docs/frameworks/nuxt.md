# Nuxt: server-loaded tasks with live Vue updates

Use a Nitro server route for direct `db` reads, Nuxt `useFetch` for the SSR
handoff, and `@hulla/db-vue` for ongoing subscriptions. Complete the
[shared task setup](../meta-frameworks.md#set-up-the-shared-database-example) first.
This recipe uses Nuxt 4's `app/` layout and Vue 3.5.

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

## Load the first rows through Nitro

The route returns the same selected rows as the browser declaration. A public
route is appropriate for this public example:

```ts
// server/api/tasks.get.ts
import { db } from '../../db/_generated/server'
import { readTasks } from '../../db/task-data'

export default defineEventHandler(() => readTasks(db))
```

Use Nuxt's SSR-aware fetch to transfer its result to the page without doing a
second page-loader request during hydration:

```vue
<!-- app/pages/index.vue -->
<script setup lang="ts">
const { data, error } = await useFetch('/api/tasks')
</script>

<template>
  <p v-if="error">Could not load the initial tasks.</p>
  <Tasks v-else-if="data" :initial-tasks="data" />
</template>
```

Nuxt discovers the component below from `app/components/`. Its live query still
fetches after mount: the page payload is a display fallback, not a confirmed live
result.

```vue
<!-- app/components/Tasks.vue -->
<script setup lang="ts">
import type { Output } from '@hulla/db'
import { useMutation, useQuery } from '@hulla/db-vue'
import { create, list } from '../../db/_generated/client/tasks'

const props = defineProps<{ initialTasks: Output<typeof list> }>()
const tasks = useQuery(list, { initialData: props.initialTasks })
const add = useMutation(create)
const addTask = () => { void add({ title: 'New task' }) }
</script>

<template>
  <p v-if="tasks.status === 'error'">Could not load tasks.</p>
  <p v-if="add.state.value.status === 'error'">Could not save the task.</p>
  <ul><li v-for="task in tasks.data" :key="task.id">{{ task.title }}</li></ul>
  <button @click="addTask">Add task</button>
</template>
```

Vue unwraps the query ref in templates; script code uses `tasks.value`. Mutation
state is `add.state.value` in script. For queries with input, pass a getter such
as `() => ({ completed: filter.value })` so the binding tracks reactive changes.

## Add private views and server writes

Authorize private Nitro routes with your provider before direct reads. Obtain
request headers through `toWebRequest(event).headers` from `h3`; Nuxt forwards
request cookies for relative server-side `useFetch`. Return a verified account
key with the rows and pass it as `initialAuthKey` to the binding. Declarations
still require their own guards and filters. Avoid cached Nitro handlers or shared
route rules for private results. See the [shared auth setup](../meta-frameworks.md#keep-server-data-private-when-adding-authentication).

Use ordinary Nitro routes for server orchestration and `db.transaction` for
atomic groups of direct writes. A browser mutation or direct server write updates
the live query independently of Nuxt's `refreshNuxtData`. The latter refreshes
Nuxt loader data when you need it, rather than managing the database subscription.

Choose a Node-compatible Nitro preset, retain the external database package and
its native executables, and keep the persistent service available for streaming.
See [deployment requirements](../deployment.md); an edge preset cannot execute
the direct server database.
