---
"@hulla/db": minor
"@hulla/db-postgres": minor
"@hulla/db-sqlite": minor
---

Add inert schema authoring with query/mutation helpers and PostgreSQL/SQLite instance constructors, immutable table extend/pick/omit composition, projected replica declarations and authorized query replica contracts. Generate per-database granular and namespace caller imports from explicit database entrypoints. Rename callable withOptimistic to optimistic. SQLite supports local-only application/server storage through Node/Bun drivers and portable WASM/Expo adapters. Typed source connections maintain authorized query coverage in read-only replicas and support durable queued server calls through mandatory device-side offline conflict callbacks, with account fences and receipt-only recovery of sent attempts. Native Rust SQLite database storage, SQLite managed HTTP hosting and full PostgreSQL native-package extraction remain separate work.

Allow database function catalogues through lazy literal module imports or build-time globs, including CommonJS declarations. Generate shared source contracts that preserve field codecs and omit configured handlers and storage settings; use these contracts for replicas and explicit source URL connections.

Keep engine settings and one optional function catalogue on application initialization. Share schema-bound declarations across independent instances, with per-instance source binding, credentials, conflict policies and lifecycle. Calls await startup, local function subscriptions react to committed writes and materialized replicas, and scope changes retire protected views. Generate metadata without evaluating application startup.

Add opt-in `functionGlobs()` from `@hulla/db/build` for Bun and Node-hosted Vite/Rollup builds. Expand literal app glob properties into module imports with the build host’s parser and filesystem APIs. Watch glob membership in Vite; validate handler ownership through ordinary instance startup before storage opens. Bind registered local declarations with `db.bind(...)` for typed calls with globs or barrels. The hook has no compiler subprocess, production dependency or database-lifecycle integration.

Load remote transport/recovery only for instances with sources. Remove redundant source listeners, duplicate catalogue alias tracking and unused SQLite write helpers. Cache SQLite column metadata and batch replica materialization within a bounded SQL parameter budget. Make shutdown reentrant and discard failed namespace bindings. Validate selected generation graphs in one bundle pass. Use the release configuration for archive tooling and include both optional drivers in release smoke imports and SQLite storage tests in CI.

Allow PostgreSQL initializer entrypoints in dev/init/serve/Studio, with optional URLs resolved from explicit settings or DATABASE_URL. Generate server-only host settings without opening eager application instances or baking environment values into client contracts. Dev manages local PostgreSQL only when no URL is supplied; invalid or unreachable connections never fall back to local storage. External provisioning and migrations remain explicit. Reject unsupported SQLite managed hosting before PostgreSQL provisioning.
