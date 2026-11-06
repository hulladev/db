# Application function globs

Barrel imports work directly, without a build plugin:

```ts
const db = sqlite({ schema, filename: './app.sqlite', functions: () => import('./functions') })
```

For application globs, add `functionGlobs()` from `@hulla/db/build` to your existing
build configuration. The hook expands literal glob properties into module imports
using the build host's parser and filesystem APIs. It adds no production dependency
and does not require Hulla's packaged compiler or Bun executable.

```ts
// vite.config.ts — the same plugin also works with Rollup
import { defineConfig } from 'vite'
import { functionGlobs } from '@hulla/db/build'

export default defineConfig({ plugins: [functionGlobs()] })
```

```ts
// Bun
import { functionGlobs } from '@hulla/db/build'

await Bun.build({
  entrypoints: ['./app.ts'],
  outdir: './dist',
  target: 'browser',
  plugins: [functionGlobs()],
})
```

Vite/Rollup runs the hook after TypeScript/JSX compilation. Rollup applications
supply their usual language plugins; Bun uses its own parser directly.

```ts
// app.ts
const db = sqlite({
  schema,
  filename: './app.sqlite',
  functions: './functions/**/*.{ts,cjs}',
})

const list = db.bind(listDrafts) // Import the declaration as usual.
const drafts = await list()
```

Use a quoted relative glob directly on a `functions` property. An imported options
object works too: its literal property is expanded in that module. Paths start
with `./` and resolve relative to the property’s file. For computed selectors or
constants holding the glob, use a barrel loader instead.

Every matched module is imported normally, including its dependencies and side
effects when the application runs. The build does not execute those modules.
Only query/mutation exports become registered functions. Runtime validation rejects
foreign-schema declarations before opening storage, including declarations produced
by application factories. Narrow your glob to the intended function modules.
Test/spec/declaration files, test directories, generated files, hidden directories
and `node_modules` are excluded. An empty glob or a namespace collision fails the
build. Namespaces follow file paths: `./functions/drafts.ts` produces
`db.functions.functions.drafts`. Use `db.bind(declarationOrNamespace)` for typed
calls; glob strings cannot infer those property names for TypeScript.

Vite watches selected files and glob roots, including additions/removals and
recovery from an empty glob. The hook only manages file selection. Database
startup, shutdown and application lifetimes use the normal instance API.
Bun compiles each `Bun.build` invocation without adding a watch service.

Managed-service generation already resolves globs and does not require this plugin.
Browser apps still supply their chosen [SQLite driver](sqlite.md#browser-and-expo-storage).
