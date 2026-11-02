# `@hulla/db-studio`

Optional local database workbench for `@hulla/db`. Start with a relationship diagram,
open a table to browse and filter its rows, or search the schema catalog for fields,
indexes and reference rules. Select rows on the current page to export JSON or
delete them together, and edit fields through managed database transactions.
Studio uses database scalar representations, so
int64 values remain lossless decimal strings and JSON stays structured.

The interface uses `@hulla/style`, `@hulla/ui`'s semantic theme and native control
adaptations, with self-hosted Schibsted Grotesk and IBM Plex Mono. Light and dark
themes follow the system initially; the header toggle saves your preference.
No framework runtime or component generator is loaded in the browser or server.

Install `@hulla/db-studio` alongside `@hulla/db`, then run:

```sh
hulla-db dev --studio
# Or connect to an already running database without starting a function service:
hulla-db studio db.config.ts
```

Open the access link printed in the terminal. Studio binds `127.0.0.1:4983`;
`--studio-port 4984` selects another port. Use `--read-only` to disable editing.
With `NODE_ENV=production`, Studio defaults to read-only; `--write` explicitly
enables managed row edits and deletions. The package and listener are loaded only when requested.

Studio is an operator interface. It bypasses application business authorization;
its access link grants access to all tables in the configured schema. Keep that
link private. Browser requests require its per-session token, and the listener
rejects foreign origins and hostnames. There is no public remote listener or SQL
console. For a production database, run Studio locally with a private database
connection, or run it on the host and forward the same loopback port over SSH.

Saving checks the loaded row snapshot inside a serializable managed transaction.
A changed or deleted row produces a conflict instead of overwriting newer data.
Bulk deletion verifies every selected snapshot and deletes the batch atomically;
reference restrictions and cascades still apply. Changing the page, sort or filters
clears selection. Read-only mode retains selection and export.
Primary keys cannot be edited. Constraints, codecs and synchronization revisions
still apply; direct edits do not invoke application query/mutation declarations
or create browser recovery receipts.

See the [Studio guide](../../docs/studio.md) for the checkout demo, architecture
and validation commands.

Licensed under MIT.
