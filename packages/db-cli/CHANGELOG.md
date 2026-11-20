# @hulla/db-cli

## 0.3.4

### Patch Changes

- ceaacaa: Keep `@hulla/db` portable and move PostgreSQL bridge and service assets to
  `@hulla/db-postgres`. Install `@hulla/db-cli` as a development dependency for
  generation and build plugins. It uses the developer's Bun installation and its
  own pinned TypeScript dependency. Install `@hulla/db-local-postgres` to opt into
  a bundled local PostgreSQL server. For Expo SQLite, use
  `@hulla/db-sqlite/expo` with the database filename and install the matching
  Expo native peers through Expo CLI.
- Updated dependencies [ceaacaa]
  - @hulla/db@0.3.4
  - @hulla/db-postgres@0.3.4
  - @hulla/db-local-postgres@0.3.4
  - @hulla/db-studio@0.3.4
