# hulladb_server

Authenticated HTTP and SSE hosting and native bridge executables for hulladb.

Since 0.1.1 the library reexports `hulladb_postgres::hosting`, also available through
`hulladb::hosting` with feature `hosting`. Existing paths and types remain supported;
this crate keeps the native executable entrypoints.

Enable `typescript` to build the `hulladb-bridge` and `hulladb-service` executables.
JavaScript handlers use the separately installed `hulladb-js` executable.

See the [repository](https://github.com/hulladev/db) for setup, examples and the architecture.
The application-facing Rust SDK is [`hulladb`](https://docs.rs/hulladb).

Licensed under MIT.
