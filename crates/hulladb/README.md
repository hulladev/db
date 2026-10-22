# hulladb

Typed PostgreSQL database authoring and durable remote function clients.

```toml
[dependencies]
hulladb = "0.1.0"
```

For a remote-only client, use `default-features = false` and `features = ["remote"]`.
The default `server` feature provides trusted PostgreSQL access.

See the [repository](https://github.com/hulladev/db) for setup, examples and the architecture.
The application-facing Rust SDK is [`hulladb`](https://docs.rs/hulladb).

Licensed under MIT.
