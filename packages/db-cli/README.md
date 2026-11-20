# @hulla/db-cli

Install this development dependency alongside `@hulla/db` for schema and client
generation, build integration, and local provisioning. For PostgreSQL commands,
also install `@hulla/db-postgres`.

```sh
npm install --save-dev @hulla/db-cli
npx hulla-db generate db/server.ts
```

Generation uses Bun and the CLI's pinned TypeScript dependency. Install Bun on
your development and build machines. Node can launch the command when `bun` is
on PATH.

For a managed local database, add `@hulla/db-local-postgres` as a development
dependency. When you already have a PostgreSQL URL, supply it to `hulla-db dev`.
Deploy the generated client or server artifacts with their runtime packages.
See the [deployment guide](../../docs/deployment.md).
