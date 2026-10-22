# Deploy an application with @hulla/db

Deploy your application using its ordinary framework workflow. Server routes use
the generated database directly; browser queries and mutations connect to a
persistent `@hulla/db` service. Both use the same PostgreSQL database and namespace.
There is no hosting-provider selector or framework requirement in the SDK.

Install the SDK from npm in both the application and service. The same release
includes the native executables needed by supported server platforms.

## Choose hosts by their capabilities

| Component | Requirements |
| --- | --- |
| Application server | Node 22.18+ or Bun, native subprocesses, executable files and PostgreSQL connectivity |
| Realtime service | A persistent Node/Bun process, native subprocesses and streaming HTTP connections |
| Database | PostgreSQL with the package's schema privileges and session settings |
| Browser | HTTP access to the service; no PostgreSQL connection or native binary |

Containers and virtual machines can host both server components. Function hosts
can host application routes when their runtime supports these requirements; run
the realtime service on a persistent host. A runtime that forbids native processes
can still serve browser assets or call an application API, but cannot run direct
database access or the native service itself. SQLite services are not substitutes
for PostgreSQL.

Deploy the same generated schema and function artifacts to the application and
service. Keep credentials in server configuration. Use an exact allowed browser
origin and HTTPS ingress that preserves SSE streaming without buffering. Configure
the application's existing auth provider on both server boundaries, and notify the
browser SDK through `setAuth` when its auth state changes.

For managed PostgreSQL, check TLS, connection limits and session behavior against
the actual endpoint. A PostgreSQL-compatible label does not establish compatibility
with transaction poolers: session settings and connection lifetime matter. Direct
endpoints and pooled endpoints need separate validation.

## Install the packages

Install the core SDK and any framework/auth integrations from npm:

```sh
npm install @hulla/db@0.1.0
npm install --save-dev typescript@7.0.2
```

The package includes JavaScript, TypeScript declarations and native executables
for macOS arm64 and Linux x64/glibc. There are no installation-time native downloads
or runtime Cargo builds. Linux musl/Alpine, Linux arm64 and macOS x64 need separate
artifacts and execution validation. Missing native targets produce an explicit error.

Add `@hulla/db-react`, `@hulla/db-vue`, `@hulla/db-svelte` or `@hulla/db-solid`
for the application's framework, and `@hulla/db-better-auth` for Better Auth storage.
Use the same release across the application, generated artifacts and persistent
service. Package maintainers can build and validate archives using the
[release workflow](releasing.md).

## Generate at build time, serve at runtime

From the application with the package installed:

```sh
# Build environment: Bun performs discovery and handler bundling.
bun add --dev typescript@7.0.2
bun --bun node_modules/@hulla/db/dist/cli.js generate db.config.ts

# Explicit provisioning step, using an appropriate database credential.
node node_modules/@hulla/db/dist/cli.js init db.config.ts

# Persistent runtime: Node or Bun; no discovery or generation.
node node_modules/@hulla/db/dist/cli.js serve db.config.ts
```

Include the config, schema, generated artifacts and their dependencies in the
service deployment. `serve` reads prepared artifacts and verifies existing storage.
Initialization never silently resets incompatible development storage. Provisioning
and application credentials should have distinct responsibilities.

Application bundlers must retain the native files. The
[Next.js fixture](../deploy/cloud/next-app/next.config.mjs) uses an external server
package and explicit native asset tracing with standalone output. Other bundlers
should copy the same package assets using their normal packaging mechanism. Native
resolution is relative to the installed package, independent of the working directory.
`HULLADB_ENGINE`, `HULLADB_SERVICE` and `HULLADB_JS` are optional operator overrides
for separately installed compatible executables.

## Reuse connections safely

Share the generated `db` across application requests. It lazily uses up to four
native sessions per handle and drains unused sessions after ten seconds. Reused
sessions are checked before application work starts. Call `db.close()` when the
application shuts down; doing so after each request would interrupt other requests.

Outside transactions, the native bridge retires after thirty seconds without a
command. Transactions use the configured execution deadline (five seconds by
default), including waits between statements. PostgreSQL enforces matching
statement and idle-transaction bounds, so a suspended application cannot hold a
transaction indefinitely. See [execution limits](execution-limits.md) to adjust them.
Keep external requests and long waits outside transactions. These bounds concern
database idleness, not the browser's network latency.

An interrupted commit can have an uncertain outcome. Direct database operations
never replay it automatically. Platform termination also cannot guarantee that an
application receives the response; use the documented mutation confirmation flow
when invoking browser mutations. See [transaction behavior](server-database.md#group-changes-in-a-transaction).

## Verified environments and remaining limits

Validation currently covers:

| Environment | Verified behavior |
| --- | --- |
| macOS arm64, Node 22.18 and Bun 1.4 | Generated direct operations, rollback and service startup |
| Linux x64, Node 22.18, Debian Bookworm container | Installed package, native startup, CRUD, rollback and service query through the restricted handler engine |
| Node/Bun process and PostgreSQL lifecycle tests | Idle drain, concurrent use, stale-session replacement and suspended-transaction rollback |
| Next 16.4.2 standalone and Chromium | Traced native binary, SSR, route/server-action writes, live query updates, rollback and revoked-session data clearing |
| Vercel Hobby, Render Free Linux x64 and Neon PostgreSQL 17 | Hosted SSR and writes, live subscriptions, grouped rollback, anonymous rejection, verified TLS/channel binding and reconnect after a real service restart |

The executable [cloud fixture](../deploy/cloud/) was deployed to Vercel and Render
against Neon. [Recorded evidence](../deploy/cloud/proofs/2026-10-22.json) includes a
service restart followed by browser reconnection and another live update without a
page reload. All disposable test resources were deleted after validation.

These checks do not establish sustained-load behavior, transaction-pooler
compatibility or automatic provider suspension/wake behavior. Other architectures
and runtime versions require their own execution evidence.

Before claiming support for a particular host, verify the deployed native assets,
cold and reused instances, concurrent requests, connection interruptions, platform
termination and the selected database endpoint. Keep preview database namespaces,
service configuration and credentials isolated from production. The same checks
apply to every provider; choosing Next does not require choosing Vercel.
