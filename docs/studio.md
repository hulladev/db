# `@hulla/db-studio`

Studio is an optional operator workbench, shipped as `@hulla/db-studio` separately
from the SDK. It provides a table relationship map, searchable schema catalog,
table browsing with cursor pagination, field filters, scalar sorting, managed row
editing, and selection for JSON export or atomic bulk deletion. Inserting rows,
function invocation, host metrics and recovery tooling are future work.

## Try it from this checkout

From the repository root:

```sh
bun install
bun run --cwd packages/db build
bun run --cwd packages/studio build
bun --conditions=hulladb-source packages/db/src/cli.ts dev packages/studio/example/db.config.ts --studio
```

The example uses the SDK's local development database. In another terminal, add
sample data while development is running:

```sh
bun --conditions=hulladb-source packages/studio/example/seed.ts
```

Open the Studio access link from the first terminal and refresh the table. The
seed creates one project and 64 tasks; repeating it preserves existing edits.
The example requires the platform's native SDK executables and local PostgreSQL
assets. Set `DATABASE_URL` to use an existing PostgreSQL instance instead. For an
external database, explicitly initialize the example before starting development:

```sh
bun --conditions=hulladb-source packages/db/src/cli.ts init packages/studio/example/db.config.ts
```

For your own project, install Studio alongside `@hulla/db`, then run
`hulla-db dev db.config.ts --studio`. Studio reloads with the development worker,
retaining the access token across restarts. Refresh the browser to load schema
changes; row snapshots expire on a worker restart and require reloading before a save.

## Standalone and production access

```sh
hulla-db studio db.config.ts
hulla-db studio db.config.ts --studio-port 4984 --read-only
NODE_ENV=production hulla-db studio db.config.ts
NODE_ENV=production hulla-db studio db.config.ts --write
```

Standalone Studio loads the configured schema and connection, including saved local
development settings. It does not generate application code, initialize storage,
start PostgreSQL, or start the function service. A database must already be running.
Production defaults to read-only, and `--write` is an explicit operator choice.
Node and Bun both support standalone operation using the prebuilt browser assets.
`serve --studio` is not part of this first version; run standalone Studio alongside
production serving when needed.

Studio always binds IPv4 loopback. To inspect a remote database, use a private
database connection from your local machine, or run Studio on the remote host and
forward its port, for example `ssh -N -L 4983:127.0.0.1:4983 host`. Open the printed
link with `127.0.0.1` and the same port. Custom hostnames, cross-origin embedding,
and public deployment behind an ingress are not supported in this version.

## Interface and styling

The landing page draws declared references between table cards, with arrows from
foreign keys to the referenced primary key. Click a card to open its table detail;
use the zoom controls and scroll to explore larger schemas. Primary keys and all
foreign keys stay visible on compact cards. The map and its schema statistics use
metadata only: opening Studio does not read table contents.

The Schema catalog searches table names, field names, scalar types and referenced
tables. It lists indexes and unique/check rule counts, plus clickable references
and their delete behavior. Inspect schema opens the table's Schema tab without
querying its rows. The sidebar and browser history navigate between these views.

In a table, select individual rows, shift-click a range, or select the current page.
The action bar exports selected values as JSON or opens a deletion confirmation
showing the table's inbound reference policies. Selection is limited to the current
page (at most 200 rows), and clears on navigation, filtering, sorting and refresh.
Read-only connections allow selection and export while hiding deletion actions.

Studio keeps its native DOM renderer and bundles `@hulla/style` for typed visual
variants. Its semantic theme and control styling are adapted from the `@hulla/ui`
registry (revision recorded in `packages/studio/src/ui/theme.css`), including
Schibsted Grotesk, IBM Plex Mono, neutral surfaces, blue accents, focus states and
light/dark palettes. This is a native adaptation, not an installation of the
framework-specific `@hulla/ui` component templates or generator package.

Branding follows the `hulla.dev` site: its H/ monogram is copied into Studio's
favicon asset, and the wordmark uses the blue accent dot. The lime stays in the
brand mark while interactive controls use the shared blue theme. Studio keeps
compact workbench spacing rather than the website's marketing-page layouts.

The styling and font packages are build dependencies only. The build bundles the
browser helper and copies two Latin WOFF2 font files and their original licenses
into `dist`. Fonts are served from the Studio listener with a private one-day
cache, and `font-display: swap` keeps text visible while they load. The server
needs no style/font npm packages, remote font host, CSS compiler or UI framework.
Theme selection runs in a small external script before first paint, within the
same content security policy as the application. Your preference is stored locally;
operator credentials remain in tab-local session storage.

The Schema tab enhances its JSON details with Highlight.js core and the JSON
grammar. The browser fetches that code only when the tab is opened, then reuses
the loaded module when switching tables. Token colors follow the shared light/dark
theme without re-tokenizing. Plain text appears immediately and remains usable
if the optional chunk fails to load; code blocks over 128 KiB stay plain text.
The build emits a manifest of the browser entrypoint and chunks so packaged
standalone servers serve every required module without exposing arbitrary files.

## Operator boundary and managed writes

The CLI resolves Studio from the application's dependencies only when requested.
Its browser assets and HTTP operator API are separate from the native application's
HTTP/SSE listener. No Studio routes are added to application function routes.

Each run creates a random 32-byte bearer secret. The access link carries it in a URL
fragment, which the browser removes and retains in tab-local session storage.
The browser never receives the PostgreSQL connection URL. Requests must use the
listener's exact loopback Host and Origin, and API requests require the bearer.
Studio does not reuse application user authentication: its operator access can read
all declared tables and intentionally bypasses business policies.

The API limits concurrent requests and connections, input size, page size, filter
count, response size and database read budgets. Tables and fields come from the
compiled schema; filtering and ordering use the public database query builder.
There is no arbitrary SQL or arbitrary expression execution endpoint.

Rows are encoded using each field's database representation, then signed with a
per-instance HMAC snapshot. On save, Studio decodes the patch with the field codecs,
reads the current row inside a serializable transaction, verifies its signature,
and performs a managed single-row update. This prevents lost edits when row contents
change before saving or another transaction races the save. The signature compares
contents; a row changed and restored to identical contents is considered unchanged.
No row data or snapshots are accumulated in a server-side cache.

Bulk deletion verifies every selected primary key and row snapshot in one managed
transaction before deleting anything. A stale, missing or changed row rejects the
whole selection, and a restricted reference rolls back the whole batch. Cascade
and set-null reference rules still apply, including effects on unselected related
rows; the confirmation shows policies rather than an affected-row estimate. The
reported deletion count covers selected rows. Deletion cannot be undone in Studio.

Primary keys stay fixed. Defaults, constraints, indexes and references are visible
in the schema view; schema authoring remains in application code. Edits preserve
managed constraints, codecs and revisions, so live queries can observe the updates.
They do not execute business declarations or create remote mutation receipts.

## Validation

```sh
bun run --cwd packages/db build
bun run --cwd packages/studio build
bun run --cwd packages/studio typecheck
bun --conditions=hulladb-source test packages/db/tests/cli-options.test.ts packages/db/tests/dev.test.ts packages/studio/tests/*.test.ts
```

HTTP access tests run under Node and Bun. The development lifecycle test controls
the native service process boundary and uses a real Studio listener to verify schema
reload, access retention, listener replacement and shutdown. It does not simulate
database transactions. Managed editing tests use real PostgreSQL through the native
bridge and exercise codecs, pagination, constraints, stale/deleted-row conflicts,
atomic bulk deletion, restricted references, cascades and lossless int64 keys:

```sh
HULLADB_TEST_URL=postgresql://… cargo test -p hulladb_server --features typescript --test direct_bridge --locked studio_managed_operations -- --ignored --test-threads=1
```
