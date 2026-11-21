# @hulla/db-cli

## 0.4.0

### Patch Changes

- Updated dependencies [81eecde]
  - @hulla/db@0.4.0
  - @hulla/db-postgres@0.4.0
  - @hulla/db-studio@0.4.0

## 0.3.4

- Reuse managed PostgreSQL downloads across CLI releases that pin the same build.
  The cache retains verified archives for recent builds.

### Patch Changes

- ceaacaa: Keep `@hulla/db` portable and move PostgreSQL bridge and service assets to
  `@hulla/db-postgres`. Install `@hulla/db-cli` as a development dependency for
  generation and build plugins. It uses the developer's Bun installation and its
  own pinned TypeScript dependency. Managed PostgreSQL downloads a verified
  archive on first use and reuses it from the cache. For Expo SQLite, use
  `@hulla/db-sqlite/expo` with the database filename and install the matching
  Expo native peers through Expo CLI.
- Updated dependencies [ceaacaa]
  - @hulla/db@0.3.4
  - @hulla/db-postgres@0.3.4
  - @hulla/db-studio@0.3.4
