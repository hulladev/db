# Serve reactive queries and mutations

Declare database functions, start the supplied service, then import the generated
browser callables. The service owns function routes, credential verification,
transactions, subscriptions and mutation confirmation. You do not write a route for
each function or maintain an export registry.

Handlers execute in a fresh restricted
JavaScript process with database access and computation capabilities. External
services belong in your application server. See [handler execution](typescript-functions.md)
for supported dependencies and runtime limits.

Deploy the application on a host that supports Node/Bun and native subprocesses,
with this persistent service and a shared PostgreSQL database. Containers, virtual
machines and suitable function runtimes use the same SDK. Next.js is a compatibility
fixture, not a required framework or hosting provider. See [deployment and packaging](deployment.md)
for runtime requirements, package builds and the limits of current validation.

## Define the application

Start with [the schema and config setup](server-database.md#define-the-application).
Allow your browser application's origin in the config:

```ts
// db.config.ts
import { defineConfig } from '@hulla/db/config'

export default defineConfig({
  database: {
    server: { url: process.env.DATABASE_URL! },
    client: { url: "http://localhost:3001", credentials: "include" },
  },
  service: {
    port: 3001,
    origins: ['http://localhost:5173'],
  },
})
```

The listener defaults to `127.0.0.1:3001`. `service.host` can set another IP or
hostname. Origins must be exact HTTP(S) origins; wildcards are not accepted. The
default empty list accepts requests without an Origin header. For remote browser
access, terminate HTTPS at your deployment's reverse proxy.

Generate the constructors before authoring functions:

```sh
bun --bun hulla-db generate db.config.ts
```

Then declare functions beside the schema:

```ts
// db/tasks.ts
import { query, mutation } from './_generated/functions'
import { z } from 'zod'

export const list = query(ctx =>
  ctx.db.select('tasks').where({ completed: false })
)

export const create = mutation({
  input: z.object({ title: z.string().min(1) }),
  handler: (ctx, input) => ctx.db.insert('tasks', input),
})
```

This example uses Zod as the application's Standard Schema validator. It is not a
required database dependency. Function discovery includes query/mutation declarations;
ordinary exported helpers do not become endpoints.

Initialize storage explicitly, then start development:

```sh
bun --bun hulla-db init db.config.ts
bun --bun hulla-db dev db.config.ts
```

`dev` uses Bun to generate bindings and the private handler bundle, start the service,
and restart it after source changes. Each restart uses a fresh process so changed
configuration and declarations are reloaded. It watches the config directory and
any separately configured schema/function directories. Generated files, dependencies,
Git metadata, logs and common framework/build/cache directories do not trigger
restart loops. Imported helpers outside these roots and external symlink targets
require a manual restart. Invalid edits stop that attempt; the
watcher remains available for the next correction. Development never initializes or
resets storage; schema changes may require explicit provisioning of development data.

For startup from prepared artifacts, generate during your build step, then use
`serve` in the runtime you choose:

```sh
# Build step: Bun is required.
bun --bun hulla-db generate db.config.ts

# Runtime step: choose Node or Bun.
node node_modules/@hulla/db/dist/cli.js serve db.config.ts
# bun node_modules/@hulla/db/dist/cli.js serve db.config.ts
```

`serve` loads existing artifacts and verifies existing storage. It does not run
discovery, generation, bundling or a watcher. Include `_generated/worker.js`,
`_generated/client/functions.json`, configuration, schema and their runtime dependencies
in the deployment. Missing artifacts produce an error with a generation instruction.
The auth verifier executes in the chosen Node or Bun process; handler declarations
execute in the same restricted native engine in either case.

The npm package contains compiled JavaScript and native binaries for macOS arm64
and Linux x64/glibc. Node services require Node 22.18+; generation and `dev` require
Bun and the TypeScript 7.0.2 development dependency. Application database calls and
`serve` contain no Bun runtime dependency, so a Node application can generate in
a separate Bun build environment.

The packaged `hulla-db` executable uses Node. Use `bun --bun hulla-db generate`
and `bun --bun hulla-db dev` for build and development commands. Commands default
to `db.config.ts` in the current directory.

`HULLADB_SERVICE`, `HULLADB_ENGINE` and `HULLADB_JS` optionally select separately
installed compatible executables. Ordinary installations use the binaries bundled
in `@hulla/db`; see [supported platforms](deployment.md#install-the-packages).
Ctrl-C or SIGTERM shuts down the service and closes its subscriptions.

## Connect the browser

Generated callables use `database.client` automatically. Imports do not open connections:

```ts
import { dispose } from './db/_generated/client/runtime'
import { list, create } from './db/_generated/client/tasks'


const stop = list.subscribe(state => {
  if (state.status === 'success') renderTasks(state.data)
  if (state.status === 'error') showError(state.error)
})

await create({ title: 'Write the guide' })
// The live query refreshes after the committed write.

// When this application instance is torn down:
stop()
await dispose()
```

`renderTasks` and `showError` represent your application's rendering code. Plain
JavaScript can observe snapshots directly; framework adapters can connect these
snapshots to their state systems. Importing bindings and configuring them do not
fetch data until a call or subscription starts. Dispose once when the application
instance ends, not after each request or render.

Direct server writes through `_generated/server` update the same storage revisions
and refresh these subscriptions too. Fresh browser loads fetch authoritative data;
there is no local SQLite replica or persistent browser mutation queue.

## Bring your existing authentication

For Better Auth storage in the same PostgreSQL database, follow the separate
[Better Auth setup](better-auth.md). Other providers use the verifier below.

The optional config `auth` callback receives the original request headers. Call
your provider's server verification method there. Return a stable account/session
key and the JSON identity fields handlers need, or `null` for anonymous access.
Throw when credentials are invalid or expired; verifier errors are not exposed to
clients. Without a callback, all calls are anonymous.

```ts
// Add to defineConfig(...). verifySession is your application's provider integration:
// it returns null for anonymous access and throws for invalid credentials.
auth: async headers => {
  const session = await verifySession(headers)
  if (!session) return null
  return {
    key: session.user.id,
    identity: { id: session.user.id, role: session.user.role },
  }
}
```

Choose a key that distinguishes the account/session and tenant boundaries your app
needs. Return only stable claims relevant to the handlers; changing the returned
identity changes the verified authorization context and retires live delivery.
The service rechecks credentials before live delivery and periodically while
subscribed. It does not refresh provider tokens. A stalled verifier is bounded and
cannot permanently consume the service's authentication capacity.

Generated `ctx.identity` infers the callback's identity type and includes `null`.
Config imports used for this inference are type-only: verification code and database
credentials stay out of handler and browser bundles. Enforce authorization in the
declaration or a shared extension:

```ts
const authenticatedQuery = query.extend(ctx => {
  if (!ctx.identity) throw new Error('Sign in required')
  return { user: ctx.identity }
})
```

Browser credential transport and cache identity remain separate:

```ts
import { setAuth, setHeaders } from './db/_generated/client/runtime'

// For bearer credentials, read the current token for each request:
setHeaders(() => session ? { authorization: `Bearer ${session.token}` } : {})

// Call from your auth library's existing change listener:
setAuth(session ? { key: session.user.id } : null)
```

For provider cookies, set `database.client.credentials` to the appropriate mode.
The callback reads current credentials for each request. `setAuth` does not send
proof of identity; spoofing its key cannot grant access. After an account change,
reattach the UI's subscriptions for the new scope. See [auth state and optional
expiry](../packages/db/README.md) for promises and `expiresAt`.

Direct server database calls retain application-owned authorization and need no
auth binding. This service does not add an action API, job scheduler, provider SDK
or framework-specific session machinery.

## Keep live views consistent

Actively observed queries in one TypeScript runtime share a live stream. Each update
evaluates the captured query set in one PostgreSQL snapshot. The SDK validates the
complete batch, updates query handles and optimistic bases, then notifies observers.
A list and count therefore publish successful current results from the same database
state. Separate runtimes and tabs are independent; a slow query delays its batch.

Adding or removing a query replaces the stream's captured set; results from the old
stream cannot publish afterward. One-shot reads resolve their own await without
independently replacing an observed query. A connection failure retains the previous
complete view as stale. A handler error affects only its query and can recover after
a database change. Access failures clear affected data even when the subscription
uses `keepPreviousDataOnError`.

## Current limits

This startup path uses the HTTP/SSE host with reusable admission tickets, durable
success/failure outcomes and automatic bounded cleanup. Coordinated publication is
implemented for ordinary queries and [stable live pages](live-pagination.md) in one
TypeScript runtime. Configurable execution budgets and the remaining schema/query
features still need implementation. Native Rust subscriptions currently send singleton rounds and do
not coordinate several subscriptions into one shared view.

The current launcher uses a 5-second worker limit, 16 MiB protocol frames, a
64 MiB JavaScript heap and 512 KiB JavaScript stack per invocation, a
four-connection host pool and at most 128 live streams, with 128 distinct queries per
captured set. Round deadlines and frame budgets apply to the complete set, not to each
query independently. These development
limits are not yet wired to the agreed configurable limits API. The input/output
payload must also fit its enclosing frame.
