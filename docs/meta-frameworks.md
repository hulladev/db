# Use @hulla/db in a full-stack framework

Render the first database result on the server, then keep the browser view live.
Your framework owns routing, server functions, authentication and serialization;
`@hulla/db` owns database operations and live query results.

| Framework | UI package | Recipe |
| --- | --- | --- |
| Next.js App Router | `@hulla/db-react` | [Server Components and client hooks](frameworks/next.md) |
| Nuxt 4 | `@hulla/db-vue` | [Nitro routes and Vue components](frameworks/nuxt.md) |
| SvelteKit with Svelte 5 | `@hulla/db-svelte` | [Server loaders and reactive components](frameworks/sveltekit.md) |
| SolidStart 2 | `@hulla/db-solid` | [Server functions and Solid resources](frameworks/solidstart.md) |
| Astro with React | `@hulla/db-react` | [Request-time pages and live islands](frameworks/astro.md) |

Use the normal framework scaffold and install `@hulla/db@0.1.0` plus the matching
UI package from npm. No separate Next, Nuxt, SvelteKit, SolidStart or Astro database package
is required. For Astro, install its normal integration for the chosen UI framework.

## Set up the shared database example

These recipes use a small **public** task list. Complete the [database setup](server-database.md#define-the-application)
and [persistent browser service setup](browser-service.md) first. Put `db/` and
`db.config.ts` at the project root, including when the framework uses `src/` or
Nuxt's `app/` directory. Keep `DATABASE_URL` private.

```ts
// db/schema.ts
import { defineSchema, defineTable, v } from '@hulla/db/schema'

export default defineSchema({
  tasks: defineTable({ title: v.string().min(1).max(200) }),
})
```

Generate once to create the schema-bound constructors. Then add a database helper
that both the server page and the browser query can reuse:

```ts
// db/task-data.ts
import type { ReadDatabase } from './_generated/types'

export const readTasks = (database: ReadDatabase) => database
  .select('tasks', ['id', 'title'])
  .orderBy('title')
  .limit(50)
```

Declare the browser functions. Ordinary exported helpers are not endpoints:

```ts
// db/tasks.ts
import { z } from 'zod'
import { query, mutation } from './_generated/functions'
import { readTasks } from './task-data'

export const list = query(ctx => readTasks(ctx.db))
export const create = mutation({
  input: z.object({ title: z.string().min(1).max(200) }),
  handler: (ctx, input) => ctx.db.insert('tasks', input),
})
```

Regenerate the bindings, initialize storage explicitly, and start the persistent
service. The examples use your service's public URL and cookie credentials. In
development that can be `http://localhost:3001`; configure the browser app's exact
origin in `service.origins`. Production requires HTTPS. A reverse proxy can expose
the service alongside the application; otherwise configure cross-origin cookie
and CORS behavior deliberately. The service URL contains no database credentials.

Opening a recipe's page renders the selected rows. Clicking **Add task** commits a
browser mutation, then the live query updates. A server route/action using
`db.insert('tasks', ...)` updates the same subscriptions. The server snapshot
itself is never a subscription, and awaiting a mutation confirms its commit
without waiting for every view to refresh.

## Keep server data private when adding authentication

The public example intentionally has no session checks. Before adapting it for
private rows, authorize both the direct server read and every query/mutation
declaration, using the same row/tenant filters. A browser component, hidden button
or server-rendered login check cannot protect a public declaration.

Use your provider's server verification method with the framework's request
headers. Better Auth's concrete call is `auth.api.getSession({ headers })`:

| Framework boundary | Request headers |
| --- | --- |
| Next server page/action | `await headers()` from `next/headers` |
| Nuxt Nitro handler | `toWebRequest(event).headers` from `h3` |
| SvelteKit server loader/action | `event.request.headers` |
| SolidStart server function | `getRequestEvent()?.request.headers` from `solid-js/web` |
| Astro page/endpoint | `Astro.request.headers` / `context.request.headers` |

Reject missing sessions before direct `db` operations. Return only authorized
selected rows and an `initialAuthKey` built from the verified user/account/tenant.
Pass that key alongside `initialData` to the UI binding. Use the same key in the
browser provider listener's `setAuth(...)`; do not obtain it from an untrusted
query parameter. On sign-out use `setAuth(null)`. Credentials still travel
separately, and the service independently verifies them on live delivery.

For example, change a binding's options to
`{ initialData: initialTasks, initialAuthKey: verifiedAccountKey }`. The key does
not authorize anything; it prevents an SSR fallback from being reused for another
account. Protected responses must also avoid shared/CDN/framework caching. See
the [complete Better Auth setup](better-auth.md) for storage, the config verifier
and a framework-independent session subscription. Put its browser subscription
in the startup hook shown by your framework recipe, not in each task component.

## Choose snapshots or live views

Use server routes/actions for application orchestration and direct database work.
Use the query/mutation declarations when the browser needs database calls and
reactive results. Share ordinary helpers taking `ReadDatabase`/`WriteDatabase`
when both paths need the same database logic. Group server writes with
`db.transaction`; keep external effects outside retryable declarations.

`initialData` is a stale, observer-local fallback. It renders without a network
request, is replaced by the first live result, and retires on input/account
changes. It never enters the authoritative shared cache. Rows in these recipes
contain strings so every framework can serialize them directly; codecs, Dates
and other representations must follow the framework's serialization rules.

The UI bindings also support [live pagination](framework-bindings.md#load-more-pages-in-a-component).
Pass the cursor mapper to the paginated hook; for SSR, pass its flattened initial
rows rather than treating a server cursor as a confirmed browser subscription.

## Deploy the application and service

Direct server database access requires Node/Bun, PostgreSQL connectivity and
permission to spawn the packaged native executables. Choose a compatible
framework adapter/preset; an edge isolate cannot run direct `db` calls. The
browser can be served from any suitable host, including a static host if it does
not need request-time server reads.

Generate during the build with Bun, package the native files for the target
platform, and run the persistent service where streaming responses are supported.
Bundled framework output must retain those native assets; externalizing a package
alone does not copy its executables. Share the server `db` across requests and
close it at application shutdown, not at the end of every request. See
[deployment and packaging](deployment.md) for assets, TLS and runtime requirements.
