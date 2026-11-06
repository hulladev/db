# Schema authoring, application instances and SQLite

Import `defineSchema`, `defineTable` and validators from `@hulla/db`. Schema
imports are inert. Initialize an application database with `sqlite({...})` from
`@hulla/db-sqlite` or `postgres({...})` from `@hulla/db-postgres`.

TypeScript supports local SQLite storage, authorized query downloads into
readonly replicas, and durable queued server mutations with explicit conflict
callbacks. A native Rust SQLite database driver, SQLite managed HTTP hosting and
complete relocation of PostgreSQL native assets remain separate work. The Rust
client's existing SQLite result/recovery cache is a different capability.

## Reusable declarations

```ts
// db/schema.ts
import { defineTable, defineSchema, v } from '@hulla/db'

const drafts = defineTable({ title: v.string(), body: v.string() })
const publications = drafts.extend({ publishedAt: v.timestamp() })
const summaries = publications.pick('title', 'publishedAt')

export const schema = defineSchema({ drafts, publications, summaries })
export const { query, mutation } = schema
```

Standalone `.extend()`, `.pick()` and `.omit()` return independent immutable table
definitions. An override replaces its validator. Removing primary keys or fields
used by inherited checks, indexes or unique constraints rejects. An implicit UUID
`id` remains available on independently composed tables.

`schema.tables` supplies bound table references and projections. Bound tables
cannot invent columns or become independent tables through composition; use the
standalone definitions for that. Schema keys name tables. Sharing definitions
shares no rows or connections.

Author functions in ordinary modules:

```ts
// app/functions/drafts.ts
import { query } from '../../db/schema'

export const listDrafts = query(({ db }) => db.select('drafts'))
// Equivalent: schema.query(...)
```

The helpers produce declarations without registering handlers on the schema.
Queries receive readonly database access; mutations receive their own scoped
transaction. Runtime instances supply `ctx.db`; handlers must use that context
rather than capture an application connection. Local execution currently supplies
`identity: null`; the managed service supplies its verified invocation identity.

## Initialize once per application

```ts
import { sqlite } from '@hulla/db-sqlite'
import { schema } from './db/schema'

export const db = sqlite({
  filename: './app.sqlite',
  schema,
  functions: () => import('./app/functions'),
})

// Calls wait for startup automatically.
const drafts = await db.functions.drafts.listDrafts()
```

Construction starts initialization eagerly. `db.ready()` optionally waits for it.
`db.state` and `db.subscribe(listener)` expose `initializing`, `ready`, `error`
and terminal `closed` states. `db.close()` is optional application shutdown, not a
per-query operation; closing does not reopen the instance. Close after an explicit
transaction finishes. The instance drains its managed local handlers during close.

Ordinary startup opens existing storage and verifies its schema. Use
`sqlite.initialize({...})` or `postgres.initialize({...})` for explicit first-time
provisioning; these return the same instance interface. Startup never silently
resets data or applies migrations. SQLite schema migrations are not implemented.

For local-only CRUD, omit `functions`:

```ts
const db = sqlite({ schema, filename: './app.sqlite' })
await db.insert('drafts', { title: 'Draft', body: 'Text' })
```

PostgreSQL uses the same structure:

```ts
import { postgres } from '@hulla/db-postgres'

const db = postgres({
  url: process.env.DATABASE_URL,
  namespace: 'app',
  schema,
  functions: () => import('./app/functions'),
})
```

The PostgreSQL URL is optional when using the development launcher:

```sh
# Omitted URL: manage persistent local PostgreSQL and run the development service.
npx hulla-db dev ./db/server.ts

# Explicit environment URL: run the same service against an existing database.
DATABASE_URL='postgres://localhost:5432/app_dev' npx hulla-db dev ./db/server.ts
```

The entrypoint exports its `postgres({...})` instance. An explicit `url` overrides
`DATABASE_URL`; an empty/invalid or unreachable URL errors instead of provisioning
another database. External storage needs explicit initialization/migration. After
generation, `hulla-db init ./db/server.ts` and `serve ./db/server.ts` also accept
the initializer entrypoint. Server settings preserve runtime environment reads
without opening the application's instance or entering browser artifacts.

An ordinary application instance outside dev requires `url` or `DATABASE_URL` and
never starts PostgreSQL itself. The dev launcher owns its local PostgreSQL process;
shutdown stops it and preserves data beside the entrypoint under `.hulla/`. SQLite
managed HTTP serving is still unsupported and rejects before any PostgreSQL starts.

The same schema and handler catalogue can be registered by independent instances.
Their connections, caches, policies and lifetimes remain separate. Functions bound
to another schema reject before opening a driver. For different schemas, share
ordinary business logic or factories producing fresh declarations for each schema.

Local query subscriptions react to committed writes through their instance and
replica materialization. They do not monitor arbitrary external SQLite connections.
SQLite serializes complete local handler invocations and bounds their pending
queue. Explicit transaction callbacks use only the supplied `tx`, await every
operation, and must finish before root access or close. Errors still abort a
transaction when application code catches the individual failed operation.

## Compose one function catalogue

Use ordinary barrel exports to combine shared and application functions:

```ts
// app/functions/index.ts
export * as tasks from '@acme/tasks/functions'
export * as drafts from './drafts'
```

Flat exports and explicit aliases also work. A shared declaration must use the
same schema as its destination instance. Generated flat/namespace aliases share
one callable; local catalogues preserve alias identity after loading.

Registration accepts one lazy module import or one build-time glob, without
arrays or implicit merging. Omitting registration exposes no nearby handlers.
For legacy declarations, generation also accepts CommonJS named exports:

```ts
const db = sqlite({
  schema,
  filename: './app.sqlite',
  functions: './functions/**/*.{ts,cjs}',
})
```

Globs resolve relative to the file declaring the `functions` option. Managed-worker
generation compiles them automatically. Direct applications enable
[`functionGlobs()` from `@hulla/db/build`](build.md) in their Bun or Vite/Rollup build;
the integration replaces literal glob properties with module imports without executing
application startup during the build. An uncompiled glob rejects before storage
opens. No filesystem scanning occurs in the running application.

For typed local calls, bind a declaration or namespace to the instance:

```ts
import { listDrafts } from './app/functions/drafts'

const list = db.bind(listDrafts)
const drafts = await list()
```

Binding does not register a function. The selected catalogue must include it, and
its schema must match the instance. A barrel import loader also infers typed
`db.functions` properties directly. Glob paths cannot describe those property types
to TypeScript; use `db.bind(...)` when authoring typed calls with a glob.

## Browser and Expo storage

Applications supply a dedicated driver connection and manage their chosen SQLite
runtime's own setup. `@hulla/db-sqlite` does not download a WASM runtime or add an
Expo SQLite dependency.

```ts
import { sqlite, wasmSQLiteDriver } from '@hulla/db-sqlite/browser'

const db = sqlite({
  schema,
  open: () => wasmSQLiteDriver(new sqlite3.oo1.OpfsDb('/app.sqlite', 'c')),
})
```

```ts
import { sqlite, expoSQLiteDriver } from '@hulla/db-sqlite/expo'
import * as SQLite from 'expo-sqlite'

const db = sqlite({
  schema,
  open: async () => expoSQLiteDriver(await SQLite.openDatabaseAsync('app.sqlite')),
})
```

The root SQLite package selects the browser entrypoint in browser bundles. OPFS
availability and workers belong to the chosen WASM runtime. Platforms without Web
Crypto supply a maintained CSPRNG through `randomBytes`.

SQLite supports scalar/JSON columns, defaults, codecs, UUIDs, scalar filters,
ordering, CRUD, bulk writes, upserts and ungrouped aggregates. Exact `int64` values
cross the driver boundary as decimal text. JSON predicates, insensitive/pattern
matching, grouped aggregates, pagination and custom key handlers currently reject
as unsupported. The transaction deadline defaults to five seconds; configure
`transactionTimeoutMs` from 1 to 60000.

## Authoritative queries and readonly replicas

```ts
// db/primary-schema.ts
import { defineSchema, defineTable, v } from '@hulla/db'

export const schema = defineSchema({
  tasks: defineTable({ workspace: v.string(), title: v.string(), internalNotes: v.string() }),
})
export const publicTasks = schema.tables.tasks.omit('internalNotes')
```

```ts
// app/functions/tasks.ts
import { schema, publicTasks } from '../../db/primary-schema'

export const myTasks = schema.query({
  replica: publicTasks,
  handler: ({ db, identity }) => {
    if (typeof identity !== 'string') throw new Error('Sign in required')
    return db.select(publicTasks).where({ workspace: identity })
  },
})
```

Authorization and row membership belong to the server handler. Replica queries
return an actual selection of their declared source/projection; widened, mapped,
forged or modified row arrays reject. Projections retain the primary key.

Generate from a configured entrypoint, such as `db/server.ts`, to obtain
`db/_generated/server`. Generation reads typed registrations and imports the inert
schema and selected declarations, without importing application startup. Keep
schemas exported from a separate authoring module. Generated browser bindings
contain type-only handler imports; `_source.js` preserves schema codecs and omits
handlers, storage URLs and connection openers. Prepared hosting reads that metadata
without running the initializer. Native managed workers still restrict handler
code to supported computation/validation capabilities.
Treat schema and codec modules as shared code; keep credentials and server-only
application code in initializer or handler modules.

```ts
// db/device-schema.ts
import { defineSchema, defineTable, v } from '@hulla/db'
import { source as primary } from './_generated/server/source'

export const schema = defineSchema({
  tasks: primary.tables.tasks.omit('internalNotes').replica(),
  drafts: defineTable({ title: v.string() }),
})
```

```ts
// Device initialization
import { sqlite } from '@hulla/db-sqlite'
import { schema } from './db/device-schema'
import { source as primary } from './db/_generated/server/source'
import * as tasks from './db/_generated/server/tasks'

const db = sqlite({
  schema,
  filename: './device.sqlite',
  sources: [{ database: primary, url: 'https://example.com/db', auth: { key: 'alice' } }],
})
const remote = db.bind(tasks)
const rows = await remote.myTasks()
```

Generated operations are unbound references until an instance binds them. Binding
is memoized per instance and works with granular references or namespaces. An old
bound callable remains closed when another instance opens; bind the original
references to the new instance explicitly. Missing, unexpected, duplicate or wrong
sources fail before driver opening. Multiple sources are supported. `auth.key` is a
local cache partition, never server identity proof.

Replica tables reject direct writes. A local `select` reads only downloaded
coverage, without implying that the full server table is available. Overlapping
query coverage is retained as a union, with revisions selecting the latest row.
Scope changes immediately retire local protected query views and prevent reading
rows until the new scope activates. Local-only rows are not deleted by that change.

## Explicit offline calls

```ts
const rename = db.bind(tasks.rename).offline(({ input, error }) => {
  const details = error.details
  if (error.code === 'stale_task' && details && typeof details === 'object'
      && 'currentVersion' in details && typeof details.currentVersion === 'string') {
    return { action: 'retry', input: { ...input, version: BigInt(details.currentVersion) } }
  }
  return { action: 'defer' }
})
```

`.offline(resolveConflict)` queues the server call; it never runs the server
handler locally. Use optional `.optimistic(...)` for temporary query feedback.
The server still authorizes and validates every call, including explicit business
or concurrency guards. There is no default conflict policy or conflict UI.
Register the callable's explicit resolver on each new instance before recovery.
Resolvers may retry with revised input, reject, or defer to application UI.

The durable queue is FIFO per source/account. Sent attempts recover through receipt
lookup with their original request and ticket, without POST resubmission. Unknown
outcomes remain recoverable and may block later intents. Terminal outcomes remain
until explicit acknowledgement. Account changes fence reads and recovery; queue
and receipt evidence are never silently evicted.

Use `pendingMutations()`, `resolveConflict(...)`, `acknowledgeMutation(...)`,
`flush()`, `setAuth(source, valueOrPromise)` and `forgetQuery(...)` on the instance.
There are bounded snapshot and intent budgets (128 entries and 16 MiB per kind);
capacity errors require application acknowledgement or explicit removal. Transport
connectivity and query `isStale`/`isOptimistic` indicators are distinct from local
instance readiness. Per-source observable connection state remains work.
