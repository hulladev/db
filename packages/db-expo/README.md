# @hulla/db-expo

Use Hulla DB with Expo SQLite for device-owned data. Install `@hulla/db`,
`@hulla/db-sqlite`, and `@hulla/db-expo` from the same release family. Run
`npx expo install expo-sqlite expo-crypto` to select native modules for your Expo
SDK.

```ts
import { sqlite } from '@hulla/db-expo'
import { schema } from './schema'
import * as operations from './functions'

export const db = sqlite.initialize({
  schema,
  filename: 'app.sqlite',
  functions: operations,
})
```

The adapter opens and owns a dedicated native connection. The SDK generates UUIDs
for schema-defined keys and uses Expo Crypto when Web Crypto is unavailable.
Call `sqlite(...)` to verify existing storage or `sqlite.initialize(...)` to
provision it. Keep your existing filename when moving from
`@hulla/db-sqlite/expo` so you reopen the same database.

For custom connections, pass `open: () => expoSQLiteDriver(connection)` in place
of `filename`. Create a fresh, dedicated connection on every call to `open`;
the instance closes it on failure, retry, or shutdown. `randomBytes` can
override the platform provider.
