# @hulla/db-sqlite

SQLite application instances for Node, Bun, and browsers.

```ts
import { defineSchema, defineTable, v } from '@hulla/db'
import { sqlite } from '@hulla/db-sqlite'

const schema = defineSchema({ drafts: defineTable({ title: v.string() }) })
const db = sqlite({ schema, filename: './drafts.sqlite' })
await db.insert('drafts', { title: 'A draft' })
```

Schema imports are inert. `sqlite({...})` starts opening existing storage; operations
wait automatically. `sqlite.initialize({...})` explicitly provisions fresh storage.
Optional `ready()`, observable `state`/`subscribe()` and terminal `close()` expose the
instance lifecycle. Register local functions through one barrel import callback or
an application glob compiled by [`@hulla/db/build`](../../docs/build.md);
combine shared/app handlers using ordinary exports.

The browser entrypoint accepts `open`, which supplies a dedicated driver. Use
[`@hulla/db-expo`](../db-expo/README.md) for Expo apps.
Readonly replicas require explicit source URL bindings. Offline mutations queue
server calls with mandatory device-side conflict callbacks and optional optimistic
feedback. Native Rust SQLite storage and SQLite managed HTTP hosting remain work.
See [SQLite and authoring](../../docs/sqlite.md) for
examples, supported queries, build-time globs and recovery behavior.
