# @hulla/db-better-auth

```sh
npm install @hulla/db@0.1.0 @hulla/db-better-auth@0.1.0 better-auth
```

Better Auth storage and schema support for `@hulla/db`, tested with Better Auth
1.7.12 on PostgreSQL under Node and Bun. Both packages are peers; no UI framework
is installed by this adapter.

```ts
import { betterAuth } from 'better-auth'
import { hullaAdapter } from '@hulla/db-better-auth'
import { db } from './db/_generated/server'

export const auth = betterAuth({
  database: hullaAdapter(db),
  emailAndPassword: { enabled: true },
})
```

Add `...betterAuthTables()` from `@hulla/db-better-auth/schema` to `defineSchema`
before generating/initializing the database. Supply the same shared provider
options to the schema helper and Better Auth when using plugins or custom fields.

See the [complete setup](../../docs/better-auth.md) for schema bootstrap, generated
static schemas, session verification, browser auth changes and supported limits.
Better Auth owns authentication; the application owns routes and authorization.
