# hulladb_client

Generic HTTP and SSE transport for hulladb function clients.

Since 0.1.1 this crate forwards to `hulladb::http`. Existing imports and the
`http` feature remain supported. New applications can depend on `hulladb` with
`default-features = false` and `features = ["http"]` for transport alone, or
`features = ["remote"]` for the durable remote runtime.

See the [repository](https://github.com/hulladev/db) for setup, examples and the architecture.
The application-facing Rust SDK is [`hulladb`](https://docs.rs/hulladb).

Licensed under MIT.
