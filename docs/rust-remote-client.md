# Rust remote function client

The `hulladb` facade supports generic named-function HTTP/SSE calls with SQLite
result caching and durable mutation recovery. It uses the same direct functions
and query watch namespaces as the [Rust authoring API](rust-authoring.md).
The shared `hulladb_client` transport verifies HTTPS origins and bounds JSON/SSE input.

## Client-only builds

Disable the default server feature and enable `remote`:

```toml
[dependencies]
hulladb = { path = "../db/crates/hulladb", default-features = false, features = ["remote"] }
```

Without `server`, `#[query]` and `#[mutation]` emit callable functions, `NAME`,
`Input`, `Output`, query `watch()` namespaces and mutation `contract()` codecs. They omit handler bodies,
context and identity types, registration adapters, and PostgreSQL dependencies.
Table/input derives retain checked codecs, typed IDs and canonical schema metadata.
Guard server-only imports and helper implementations in the shared authored module
with the application's server feature. Cargo features unify within a build: a build
that also enables `hulladb/server` includes server code.

The runnable [remote example](../crates/hulladb/examples/remote.rs) demonstrates a
shared authored module and client-only execution:

```sh
cargo check -p hulladb --no-default-features --features remote --example remote --locked
cargo tree -p hulladb --no-default-features --features remote --edges normal --locked
```

The production dependency graph contains neither `hulladb_postgres`,
`tokio-postgres`, nor `hulladb_server`. Server integration tests still have database
dev dependencies. Cross-language generation from Rust signatures remains separate work.

## Connection and calls

Supply a fixed `Endpoint`, opaque bearer, trusted `Session` containing scope,
principal and authorization scope, and a `RecoveryStore`. `RemoteRuntime::connect`
performs a bounded authenticated handshake and verifies the stored source binding.
It requires connectivity for this handshake; it does not authenticate offline.

```rust,ignore
use hulladb::remote::{Endpoint, Options, RecoveryStore, RemoteRuntime, Session};
use std::sync::Arc;

let storage = RecoveryStore::open("functions.sqlite").await?;
let runtime = Arc::new(RemoteRuntime::connect(
    endpoint, &bearer, session, storage.clone(), Options::default(),
).await?);

hulladb::scope(runtime.clone(), async {
    let task = create(CreateTask { title: "Ship it".into() }).await?;
    let tasks = pending().await?;
    let subscription = pending::watch().await?;
    Ok::<_, hulladb::Error>(())
}).await?;
```

HTTPS verifies certificates and the origin name, including explicitly configured
private CA roots through `Endpoint::with_trusted_roots`. Plaintext endpoints require
a literal loopback address. Redirects are not followed. Request, response/event
bytes, stream idle time, subscriptions, pending writes and recovery attempts have
explicit budgets in `Options` / `ClientOptions`.

Each request carries the expected scope, principal and authorization scope in
`X-Hulladb-*` headers. The managed host checks these against authentication before
execution, preventing a remapped credential from submitting an old identity's write.
The browser SDK sends the same identity preconditions. Response envelopes are also
validated for canonical identifiers, counters, identity and source. Typed calls and
watches validate the output codec before persisting a result or acknowledging a write.
The lower-level raw `Runtime::query` / `mutation` methods return validated envelopes
with JSON results; use their `*_checked` variants when supplying a result validator.

## Subscriptions and visibility

Each query watch owns a real SSE stream. Drop aborts its worker and connection.
Complete cached query results hydrate as `Ready { connected: false, .. }`.
The SQLite store keeps at most 128 disposable snapshots and 16 MiB of encoded
partition/query keys and snapshots across all partitions, evicting least recently
refreshed entries. Oversized results still return to the caller but are not cached.
This bounds retained cache content, not SQLite file/WAL overhead or application-owned
mutation recovery records. Receipt retention remains explicit through `forget_receipt`.
Transport outages preserve those results as stale and reconnect with a new stream.
Reconnects receive authoritative replacement snapshots; there is no retained-history
claim and no local table coverage. Subscription IDs, delivery counters, event IDs,
snapshot revision ordering, UTF-8 and frame budgets are checked before caching or
publishing. Authorization, source and protocol failures terminate the identity and
clear its result cache.

`Subscription::wait_visible(&receipt, budget)` succeeds only when a connected
snapshot has the receipt's source and scope and reaches its revision. Stale cache
hydration does not satisfy a visibility fence. This proves query visibility, not
that a particular row belongs in a filtered result.

## Durable recovery and retirement

SQLite runs on blocking workers with WAL and full synchronization. Volatile
SQLite targets such as `:memory:` are rejected. A mutation's
UUID, operation, encoded input and source commit locally before network submission.
An ambiguous response retries its receipt first, then resubmits the original UUID
and input only when the receipt is absent. Attempts are bounded; exhaustion returns
`CommitOutcomeUnknown`. Cancellation preserves the journal and never asserts that
a server transaction was cancelled.

After restart, recover explicitly using each stored operation's current contract.
Unknown operations and incompatible inputs are refused before any receipt lookup
or resubmission:

```rust,ignore
runtime.recover_pending(&[create::contract()]).await?;
```

`pending()` exposes unresolved intents. Denied recovery, unknown contracts,
malformed results and source mismatches preserve them. A definite rejection of an
initial submission can remove that new intent; rejection during recovery cannot.
A validated receipt and its original intent are saved atomically when the pending
entry is removed. `RecoveryStore::completed(partition)` therefore exposes confirmed
operations even if the caller was cancelled at the acknowledgement boundary.
Applications control local receipt retention with `forget_receipt`; server receipt
retention and deduplication are separate concerns. Explicit server compaction
returns `Error::ReceiptExpired` (HTTP 410): recovery stops, keeps the pending intent
and never resubmits it. Reconcile business state without allocating a replacement
request automatically; see [receipt retention](operations-visibility.md).

Results and journals are partitioned by normalized origin, scope, principal and
authorization scope. Credentials are never persisted. Keep one owner for an active
identity; await `retire()` before replacing credentials or claims. Retirement aborts
transport, prevents late persistence/publication, clears that partition's cached
results and preserves unresolved intents for their original identity. It does not
transfer writes to the replacement identity.

A changed server source requires explicit reconciliation. Retire the old owners,
inspect and resolve their intents, and only then call
`RecoveryStore::reconcile_source(partition, new_source)`. It refuses while unresolved
intents remain. `discard_intent` is an explicit application/operator decision after
investigating an outcome; it does not undo a server commit. Incompatible development
SQLite files fail without resetting data. Preserve/reconcile their intents and
explicitly recreate fixtures when changing the storage format; no compatibility
versions or upgrade migrations are maintained before publication.

## Validation

`make remote-functions-check` runs the client-only contract and failure/recovery
cases. `make remote-functions-test` additionally runs the real PostgreSQL host test.
The shared SQLite dependency requires rusqlite 0.39 (bundled SQLite 3.51.3):
concurrent recovery testing reproduced the Unix lock-order deadlock in SQLite
3.51.1, [fixed upstream in 3.51.2](https://sqlite.org/releaselog/3_51_2.html).
The cases cover lost acknowledgements, cancellation/restart, denied recovery,
same-UUID resubmission, source replacement, identity remapping, typed result
validation, stale hydration, live visibility, malformed/oversized SSE, capacity,
retirement, regressed live revisions, envelope budgets, denied restart handshakes
and preservation of incompatible local storage.
