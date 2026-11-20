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

With no PostgreSQL URL configured, `hulla-db dev` downloads and caches a verified
PostgreSQL build on first use. SDK releases that use the same build share its
cached archive. Managed PostgreSQL supports macOS arm64 and Linux
x64 with glibc. Set the server URL or `DATABASE_URL` to use an existing database.
Deploy the generated client or server artifacts with their runtime packages.
See the [deployment guide](../../docs/deployment.md).
