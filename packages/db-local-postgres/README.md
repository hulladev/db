# @hulla/db-local-postgres

Install this development dependency to let `hulla-db dev` provision and manage
a local PostgreSQL database. It contains the PostgreSQL binaries and data files
for the supported local platforms.

```sh
npm install --save-dev @hulla/db-cli @hulla/db-local-postgres
npx hulla-db dev
```

Install `@hulla/db-postgres` for PostgreSQL execution. With a configured
PostgreSQL URL, the CLI uses that connection and the local asset package is optional.
