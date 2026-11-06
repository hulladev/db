# @hulla/db-postgres

PostgreSQL application instances using shared `@hulla/db` schemas and execution.

```ts
import { defineSchema, defineTable, v } from '@hulla/db'
import { postgres } from '@hulla/db-postgres'

const schema = defineSchema({ tasks: defineTable({ title: v.string() }) })
const db = postgres({ schema, url: process.env.DATABASE_URL, namespace: 'app' })
await db.insert('tasks', { title: 'A task' })
```

Schema imports are inert; `postgres({...})` starts the instance. Optional `functions`
selects one handler barrel or build-time glob. Handlers use `ctx.db` and can be shared
between instances of the same schema. Optional `ready()`, `state`/`subscribe()` and
terminal `close()` expose lifecycle. Use `postgres.initialize({...})` for explicit
fresh provisioning. PostgreSQL cannot store device replicas.

`url` takes precedence over `DATABASE_URL`. Run `hulla-db dev ./db/server.ts` against
an exported initializer to start the development service. Without either URL, dev
starts and initializes its own persistent PostgreSQL; with a URL, it starts the
same service using that database. External databases require explicit `init` or
`migrate`. Invalid or unreachable URLs never fall back to managed storage.

Outside the development launcher, opening an instance needs `url` or `DATABASE_URL`;
the adapter never starts a PostgreSQL server. Generation prepares server-only
settings without opening the instance, and evaluates environment URLs at hosting
time. Deploy the generated directory along with the application.

This adapter currently delegates to the core PostgreSQL bridge. Complete native
asset relocation remains work. See [schema authoring and SQLite](../../docs/sqlite.md).
