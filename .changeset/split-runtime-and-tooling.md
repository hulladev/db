---
"@hulla/db": patch
"@hulla/db-postgres": patch
"@hulla/db-sqlite": patch
"@hulla/db-cli": patch
"@hulla/db-local-postgres": patch
---

Keep `@hulla/db` portable and move PostgreSQL bridge and service assets to
`@hulla/db-postgres`. Install `@hulla/db-cli` as a development dependency for
generation and build plugins. It uses the developer's Bun installation and its
own pinned TypeScript dependency. Install `@hulla/db-local-postgres` to opt into
a bundled local PostgreSQL server. For Expo SQLite, use
`@hulla/db-sqlite/expo` with the database filename and install the matching
Expo native peers through Expo CLI.
