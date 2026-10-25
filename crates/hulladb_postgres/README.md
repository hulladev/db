# hulladb_postgres

Transactional PostgreSQL function runtime, authorization and recovery for hulladb.

Enable `http` for `hulladb_postgres::hosting`, the authenticated HTTP/SSE host.
The default build keeps the HTTP server stack disabled. `typescript` enables the
restricted worker bridge; `host-typescript` also enables HTTP hosting and native
service/bridge entrypoint support. The `hulladb_server` compatibility crate
continues to provide the original library paths and executables.

See the [repository](https://github.com/hulladev/db) for setup, examples and the architecture.
The application-facing Rust SDK is [`hulladb`](https://docs.rs/hulladb).

Licensed under MIT.
