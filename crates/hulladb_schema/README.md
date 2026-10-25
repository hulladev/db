# hulladb_schema

Language-neutral schema, validation and lossless value contracts for hulladb.

The shared implementation also includes `protocol` for portable function/transport
contracts and `wire` for lossless tagged JSON records. The `hulladb_protocol` and
`hulladb_wire` crates preserve their existing paths through reexports.

See the [repository](https://github.com/hulladev/db) for setup, examples and the architecture.
The application-facing Rust SDK is [`hulladb`](https://docs.rs/hulladb).

Licensed under MIT.
