# Portable application compatibility fixture

This tests a Node application, a separately running persistent `@hulla/db` service,
and PostgreSQL. Next.js is one framework example; the database package has no
Vercel dependency. The same package supplies JavaScript and native processes to
Node or Bun hosts that permit child processes. Edge/isolate runtimes do not meet
that requirement. No cloud account, hosted PostgreSQL or deployment is created by
this fixture.

The Next app uses the generated API unchanged:

- A dynamic server-rendered page reads through `_generated/server`.
- A Next server action and a route handler insert directly through `db`.
- A route exercises grouped rollback with `db.transaction`.
- A browser subscribes through `_generated/client/tasks`; committed direct writes
  appear through the separate persistent service.

`lib/session.ts` is a deliberately limited fixture-owned verifier for a preexisting
opaque cookie, not an authentication product. The test harness supplies the cookie;
there is no signup/login route. An optional fixture-only session-state file lets the
harness revoke the cookie while a live query is open and check that protected rows
are cleared. Configuration refuses to run without an explicit
`FIXTURE_MODE=isolated`, a `cloud_fixture_*` database namespace, private session token,
connection URL and app origin. Use a disposable database. Replace the fixture verifier
with your provider's server verification before adapting this into an application.
Keep application, service, auth and database settings separate for each environment.

## Reproduce locally from the package

Build/package `@hulla/db` with native assets matching the local OS/architecture,
then run from the repository root:

```sh
HULLADB_TEST_URL=postgresql://user:password@127.0.0.1:5432/disposable \
PSQL=/absolute/path/to/psql \
bun deploy/cloud/verify.ts /absolute/path/to/hulla-db-0.1.0.tgz
```

The script installs the tarball into a fresh temporary application outside the
checkout. Bun generates the application at build time. Node builds Next's production
standalone output, which is copied to another directory and launched with the packaged
native bridge. The persistent service uses prepared artifacts under Node; no Cargo,
checkout binary override, code generation or dependency installation happens at runtime.
The test checks a real Chromium browser as well as HTTP/SSR and rollback results.
Install the matching Playwright Chromium beforehand, or supply `CHROMIUM_EXECUTABLE`.
`NODE_BINARY` can select Node; the default resolves `node` on PATH.

Successful runs leave `result.json` and build artifacts in the printed temporary
workspace for inspection. `PSQL` removes only the unique namespace created by the
run. Without it, the script prints the namespace for explicit cleanup. Do not point
this proof at production storage.

The browser receives only the public service URL and cache identity. Database and
cookie credentials are supplied to the servers through environment variables. The
proof scans static browser assets for their actual private values. The example
cookie is HttpOnly, and the service verifies it independently of `setAuth`.

## File tracing

`next.config.mjs` selects `output: 'standalone'`, keeps `@hulla/db` external on the
server and explicitly includes its `native/**/*` assets in server traces. Native
assets must match the deployment OS/architecture; a successful macOS build alone
cannot establish Linux compatibility. The verification script checks that the bridge
is present in standalone output and executes from that copied package.

See Next's official [standalone output and file tracing](https://nextjs.org/docs/app/api-reference/config/next-config-js/output)
and [server external packages](https://nextjs.org/docs/app/api-reference/config/next-config-js/serverExternalPackages).
This fixture uses Next 16.4.2, React 19.2.5 and Node 22.18 or newer. A local standalone
proof does not establish Vercel deployment, suspension behavior or managed PostgreSQL
pooler compatibility.

## Provider-neutral persistent service image

Copy a package tarball containing the target Linux native assets to
`deploy/cloud/service/hulla-db.tgz`, then:

```sh
docker build --platform linux/amd64 -f deploy/cloud/service/Dockerfile -t hulla-service-fixture deploy/cloud
```

The shown command targets the verified Linux x64 assets. Match both the image
platform and package native assets if testing another architecture.

The multi-stage image generates with Bun and runs with Node. It includes no Cargo
runtime or repository checkout. Supply the settings from `next-app/.env.example`
through your container platform's secret/environment mechanisms. Bind port 3001;
terminate public HTTPS at your proxy. Provision the isolated schema explicitly with
`node node_modules/@hulla/db/dist/cli.js init db.config.ts` before starting `serve`.
Startup never initializes or resets storage. SIGTERM closes the service.

This is a compatibility image for the fixture's isolated auth/storage setup. It is
not a production auth recipe or evidence of deployment to a particular provider.

## Recorded local verification — 19 October 2026

The complete installed-package proof passed with Node 22.18.0, Next 16.4.2,
React 19.2.5 and Playwright Chromium on macOS arm64. It exercised standalone native
asset tracing, authenticated SSR, route and server-action writes, transaction rollback,
live browser updates, anonymous rejection, active-session revocation clearing rows,
and credential exclusion from browser assets. The final tested package SHA-256 was
`561b57032070fe888560c93bde2b0d83bc4372862c8b16636e854d3dbf873bc5`.

The provider-neutral image was also built for Linux x64 and run with Node 22.18.0
against disposable local PostgreSQL. Packaged native direct insertion and an
authenticated restricted query passed; the container and its namespace were removed.
This separate image check preceded the final native idle-timeout adjustment. The
package's final Linux verification is recorded with the packaging/lifecycle tests.
No hosted deployment, managed database, transaction pooler or provider suspension
behavior is established by these results.

## Recorded hosted verification — 22 October 2026

The fixture now uses the generated automatic browser setup and TypeScript 7.0.2
for generation. For a hosted cookie session, set `NEXT_PUBLIC_HULLA_SERVICE_URL=/db`
and the server-only `HULLA_SERVICE_ORIGIN` to the persistent HTTPS service. Next
rewrites `/db/*` to that service so the browser sends the application's HttpOnly
cookie on its own origin. Validate streaming and reconnect behavior through the
actual provider proxy before relying on this topology.

The rebuilt package SHA-256 is
`4fdf6830e692bd3079aac67289788d623ab6c3dc5379fdc43e7406588bb53ba9`.
It contains macOS arm64 and Linux x64 native executables. Mandatory SCRAM channel
binding was verified against a new disposable Neon PostgreSQL 17 endpoint, using
the original `sslmode=require&channel_binding=require` settings. The maintained
PostgreSQL TLS adapter now supplies that binding while retaining certificate and
hostname verification.

Actual Vercel Node 22 functions in Frankfurt passed authenticated direct insertion,
SSR of committed rows, grouped rollback and anonymous rejection against that Neon
project. The full installed-package local Node/Chromium proof also passed against
Neon: standalone native tracing, browser live updates, server-action/route writes,
rollback, session revocation clearing data and exclusion of actual credentials from
browser assets. Its temporary database namespace was explicitly removed.

Railway rejected project creation during its platform outage. A dedicated private
Render fixture repository was deployed instead, on the free Linux x64 tier in
Frankfurt with Node 22.18.0. Runtime credentials were mounted through a Render
secret file and excluded from source.

A real Chromium browser passed the complete Vercel → Render → Neon path:
authenticated live subscriptions through the same-origin rewrite, server-action
and direct-route writes appearing live, grouped rollback, anonymous rejection and
exclusion of private credentials from browser assets. Restarting only the isolated
Render service replaced its instance; the browser opened a second subscription
without a reload and received a new committed write after recovery. The test
browser closed afterward. The exact image also passed local authentication and a
restricted query against Neon before deployment.

The recorded result is [proofs/2026-10-22.json](proofs/2026-10-22.json). These checks
establish this deployment topology, not sustained-load capacity, transaction-pooler
compatibility or automatic provider suspension/wake behavior. All resources use
separate disposable fixture storage and credentials. No existing application or
database was changed.

The test used Vercel Hobby, Render free compute and a Neon free organization.
After verification, the dedicated Vercel project, Render service and Neon project
were deleted. Existing projects were left intact; the proof evidence is retained.
The recorded test URLs are historical and no longer serve the fixture.
