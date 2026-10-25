# hulladb

Typed PostgreSQL database authoring and durable remote function clients.

```toml
[dependencies]
hulladb = "0.1.1"
```

For a remote-only client, use `default-features = false` and `features = ["remote"]`.
The default `server` feature provides trusted PostgreSQL access.
Enable `http` for the standalone `hulladb::http` transport without SQLite or
PostgreSQL, or `hosting` for `hulladb::hosting` HTTP/SSE serving. Shared contracts
are available under `hulladb::schema`, `hulladb::protocol` and `hulladb::wire`.

Version 0.1.1 consolidates implementation while preserving the published 0.1.0
crate paths, features, storage format and transport protocol.

See the [repository](https://github.com/hulladev/db) for setup, examples and the architecture.
The application-facing Rust SDK is [`hulladb`](https://docs.rs/hulladb).

Licensed under MIT.
