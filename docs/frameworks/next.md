# Next.js: server-rendered tasks with live updates

Use an App Router Server Component for the first read and `@hulla/db-react` for
the live browser view. Start with the [shared task setup](../meta-frameworks.md#set-up-the-shared-database-example).
This recipe uses Next 16 and React 19, with `app/` at the project root.

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

## Pass the server snapshot to a client component

The server reads the same selected shape as the browser query. This example
requests fresh rendering instead of prerendering the database result:

```tsx
// app/page.tsx
import { db } from '../db/_generated/server'
import { readTasks } from '../db/task-data'
import { Tasks } from './tasks'

export const runtime = 'nodejs'
export const dynamic = 'force-dynamic'

export default async function Page() {
  const initialTasks = await readTasks(db)
  return <Tasks initialTasks={initialTasks} />
}
```

The Client Component renders the supplied rows during SSR and the first browser
render. After hydration, the hook subscribes and receives authoritative updates:

```tsx
// app/tasks.tsx
'use client'
import type { Output } from '@hulla/db'
import { useMutation, useQuery } from '@hulla/db-react'
import { create, list } from '../db/_generated/client/tasks'

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

The type helper describes the resolved rows, not the hook's loading state. The
mutation's `.state` is available for pending/error UI; each call is independently
awaitable. A `loading.tsx` or Suspense boundary can cover the server read, while
the query snapshot covers subsequent browser loading and errors.

## Keep database work on the server

Next Server Actions and Route Handlers can import the generated server `db`.
They own request authentication and can call `db.transaction` for grouped writes.
Their writes refresh browser subscriptions without calling `revalidatePath` for
the database cache. Use Next's revalidation APIs separately if other Server
Components also need to render again; live queries do not rerender an RSC tree.

For private data, verify `await headers()` with your auth provider before the
server read, scope the query/mutation declarations independently, and pass the
verified `initialAuthKey` to `useQuery`. Do not opt private results into shared
Next/CDN caches. See [authentication boundaries](../meta-frameworks.md#keep-server-data-private-when-adding-authentication).

Keep `@hulla/db` external to the server bundle and trace the native assets:

```js
// next.config.mjs — merge into your existing config
export default {
  serverExternalPackages: ['@hulla/db'],
  outputFileTracingIncludes: { '/*': ['node_modules/@hulla/db/native/**/*'] },
}
```

This runs on compatible Node hosts; Next does not imply Vercel. Choose native
assets for the deployment platform and run the persistent subscription service
separately. See [packaging](../deployment.md) and Next's
[server external package setting](https://nextjs.org/docs/app/api-reference/config/next-config-js/serverExternalPackages).
