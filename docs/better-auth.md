# Store Better Auth data in PostgreSQL

Use `@hulla/db-better-auth` when Better Auth should store users, sessions, accounts
and verification records in the same PostgreSQL database as your application.
Better Auth owns passwords, cookies, session expiry and authentication endpoints.
The adapter supplies storage; it does not add a second authentication system.

The package is a separate, unpublished workspace package, tested with Better Auth
1.7.12. It requires `@hulla/db` and `better-auth` as peers. Using another provider
does not require this package; keep the existing [credential verifier](browser-service.md#bring-your-existing-authentication).

## Define the tables before generating your database

Start with the normal [schema and config setup](server-database.md#define-the-application).
Spread the auth tables into your application schema:

```ts
// db/schema.ts
import { defineSchema, defineTable, v } from '@hulla/db/schema'
import { betterAuthTables } from '@hulla/db-better-auth/schema'

export default defineSchema({
  ...betterAuthTables(),
  tasks: defineTable({ title: v.string() }),
})
```

Generate and explicitly initialize storage using the normal database commands.
The schema helper does not import your generated database, open a connection or
create tables itself. This lets a new project generate its database before its
auth instance exists.

For plugins, additional fields or renamed models, put the Better Auth options in
an independent `auth-options.ts` module. Pass those same options to
`betterAuthTables(authOptions)` and `betterAuth({ ...authOptions, database: ... })`.
Keep the shared module independent of the generated database and auth instance.
The configured helper discovers additional tables at runtime. For plugins whose
constructors import server code, use the static generator below: query workers
cannot bundle server imports from a shared options module. Static auth tables
also preserve precise application field types for customized models.

## Connect Better Auth to the generated server database

```ts
// auth.ts — server only
import { betterAuth } from 'better-auth'
import { hullaAdapter } from '@hulla/db-better-auth'
import { db } from './db/_generated/server'

export const auth = betterAuth({
  secret: process.env.BETTER_AUTH_SECRET!,
  baseURL: process.env.BETTER_AUTH_URL!,
  database: hullaAdapter(db),
  emailAndPassword: { enabled: true },
})
```

Mount `auth.handler(request)` on your application's `/api/auth/*` route using
Better Auth's normal framework recipe. It accepts a Web `Request` and returns a
`Response`; the storage adapter needs no framework-specific counterpart. Existing
Next, Nuxt, SvelteKit, Astro and other application auth routes retain their own
request lifecycle. Direct `db` calls use the application's authorization checks.

The adapter supports transactions. A Better Auth transaction callback runs once
and rolls back all its database operations if it fails. Internal, database-only
single-row operations may retry a confirmed OCC rollback; they never replay a
provider callback or an external effect.

## Verify browser database calls with the same session

If your app also uses reactive query/mutation declarations, add the provider's
session lookup to the database config. The lazy import avoids a cycle:
`auth.ts` imports the generated database, which imports this config.

```ts
// Inside defineConfig({ database: ..., auth: ... }) in db.config.ts
auth: async headers => {
  const { auth } = await import('./auth')
  const session = await auth.api.getSession({ headers })
  return session ? {
    key: session.user.id,
    identity: { id: session.user.id },
  } : null
}
```

Query/mutation handlers receive this verified `ctx.identity`. Check it in the
handler or a shared extension before reading protected rows. Auth tables are
ordinary server-side tables; adding them to the schema does not create browser
endpoints or expose their contents.

On the browser, use Better Auth's existing session state to update the generated
runtime's cache identity. This vanilla client works without a UI framework:

```ts
// app/database.ts — browser startup
import { createAuthClient } from 'better-auth/client'
import { dispose, setAuth } from './db/_generated/client/runtime'

export const authClient = createAuthClient()
const cacheIdentity = (data: typeof authClient.$Infer.Session | null) =>
  data ? { key: data.user.id, expiresAt: data.session.expiresAt.getTime() } : null

// Use database.client.url = "/" for a same-origin proxy.
setAuth(authClient.getSession().then(({ data, error }) => {
    if (error) throw error
    return cacheIdentity(data)
  }))
```

Subscribe once for the lifetime of the application:

```ts
const stopAuth = authClient.useSession.subscribe(({ data, isPending, error }) => {
  if (isPending) return
  setAuth(error ? Promise.reject(error) : cacheIdentity(data))
})

// On application teardown:
stopAuth()
await dispose()
```

Cookies travel separately with same-origin requests. Cross-origin deployments
need matching cookie, CORS and `credentials: 'include'` settings. Bearer-based
providers can use the existing request-header callback instead. Cache keys never
authenticate a request. Include an active tenant/account in both keys when your
app supports switching between them; changing the key retires the previous live
view. `expiresAt` is optional, and framework bindings reattach automatically.

## Generate a static auth schema when you need exact types

The adapter implements Better Auth's schema-generation hook. Its CLI generates a
module at `db/auth-schema.ts` by default, exporting `authTables`; import and spread
that object in `defineSchema`. The generated file depends only on
`@hulla/db/schema`, so it remains independent of the auth instance and generated
database. The adapter never overwrites an existing file automatically.

For tooling that already has the provider options, the same generator is public:

```ts
import { getAuthTables } from 'better-auth/db'
import { generateAuthSchema } from '@hulla/db-better-auth/schema'
import { authOptions } from './auth-options'

const source = generateAuthSchema(getAuthTables(authOptions))
// Write source to db/auth-schema.ts, review it, then regenerate the database.
```

Renamed models/fields, plugin tables, nullable values, Dates, JSON, arrays, enums,
indexes and unique constraints are preserved. Better Auth owns generated opaque
string IDs and function defaults. Numeric IDs, non-primary-key references,
unsupported deletion policies and skipped schema management fail explicitly.
Native Better Auth joins are unsupported; leave `advanced.database.joins` off to
use its portable fallback. Its offset reads use an ordered prefix and remain
subject to database row/byte budgets; application lists should use cursor paging.
