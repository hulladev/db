# Deployed function SDK adapters

For ordinary application setup, start with [the supplied service and generated
browser configuration](browser-service.md). This page describes its underlying
adapter and protocol boundaries.

`packages/db` supplies `HttpRuntime` for generated TypeScript callables.
`hulladb_server::managed::ManagedHost` serves native Rust functions and restricted
TypeScript declarations through
`hulladb_postgres::managed`. The integration uses a user-defined table, not the task
command enum. See [bootstrap](../packages/db/README.md#current-adapter-and-generator-boundary)
and [native execution](managed-functions.md).

## Generated validation

`generateFunctions` uses the TypeScript checker to compile inferred input/output
shapes into browser-safe codecs and a generated `contracts` manifest. Authored
handlers remain type-only imports. Portable shapes include strings, booleans,
finite numbers, bigint, built-in Date, null/undefined, literals, arrays, fixed
required tuples, closed records with optional fields, and unions. Recursive/open
structures, unresolved types, any/unknown, functions and built-in container objects
are rejected. Phantom primitive brands retain their underlying structural shape;
they do not add semantic ID validators.

The wire represents bigint and revision/sequence counters as canonical decimal
strings; Dates use explicit millisecond tags. Stored int64 range checks remain
separate from arbitrary-size function bigint results. Invalid Unicode,
unknown record fields and invalid remote results fail validation. Ambiguous union
wire values (for example a decimal string that could be string or bigint) are
rejected rather than decoded into the wrong domain value. Bigint literal
signatures are rejected during generation; declare bigint instead. Network envelopes,
subscription cursors and received snapshots are validated independently of static
types. Native `FunctionContract` implementations also validate arguments/results;
generated Rust registration of TypeScript signatures remains authoring work.

## Serving and authentication

Initialize a `ManagedStore` explicitly and provision scopes before serving. Construct
`ManagedHost::connect` with the schema, registry, trusted `Authenticator`, and
`managed::Options { budgets: HostOptions, cors_origins }`. The host verifies existing
storage without installing it. Keep the host owner alive while its router is served;
`stop` or dropping it retires streams and closes the pool.

The managed authenticator receives `&CredentialHeaders`, containing the original
request headers, and returns a verified `Context<I>` with scope, principal,
authorization scope and application identity. The application chooses cookies,
bearer tokens or another credential scheme and may explicitly accept missing
credentials as an anonymous context. Invalid credentials are never automatically
downgraded to anonymous access. No membership schema is imposed.
Policies authorize queries/mutations inside their snapshot/transaction. Receipt
lookup verifies original ownership without replaying those policies. Subscription polling reauthenticates and reruns authorization before
delivery. Credentials/authorization changes terminate old streams. Applications
must change authorization scope when relevant claims or rules change.

The client accepts HTTPS, or loopback HTTP for development, and rejects URL
credentials and redirects. Fetch includes same-origin cookies by default. Configure
`credentials: 'include'` for cross-origin cookies, or a `headers` callback to read
current provider-owned tokens. The host accepts only explicit configured browser
origins, permits credentialed requests for those origins, implements preflight,
and sends no-store responses. Terminate deployment TLS in the serving infrastructure; the
PostgreSQL connector supports verified TLS; see
[database roles and transport](production-readiness.md#database-roles-and-transport). Existing
`HostOptions` bound admission, pool size, operation deadlines, request/result sizes,
subscriptions, polling and credential-check cadence. The client bounds response/frame
size, operation/idle timeouts and recovery/backoff. Production provisioning and
quotas are described in [production operations](production-readiness.md).

## Protocol

All routes pass through the trusted verifier except permitted CORS preflights;
that verifier may accept anonymous requests. JSON errors contain
only a structured `code`; internal database details are not returned.

| Route | Request/result |
| --- | --- |
| GET `/functions/session` | Head fields plus `ticket: {id, expires_in_ms, renew_after_ms}` |
| POST `/functions/query` | `{name, input, expected_source}` → head fields plus `result` |
| POST `/functions/mutate` | Query fields plus `request_id` and `ticket` → `{request_id, source, scope, revision, result}` |
| GET `/functions/receipts/{request_id}?source=UUID&ticket=UUID` | Original success, durable failure code, null if pending/absent, or HTTP 410 for expired success |
| POST `/functions/receipts/{request_id}/ack` | `{source, ticket}` → null; releases a delivered result, never its execution fence |
| POST `/functions/subscriptions` | `{expected_source, queries: [{key, name, input}]}` → SSE replacement rounds |

Identity/source UUIDs and counter strings are validated. The session handshake obtains the verified identity and storage source from the
server; browser auth metadata is never sent as proof. Subsequent requests pin that
verified context, and the server checks those assertions after authenticating. Mutations check `expected_source`
inside the locked transaction before handler execution or receipt replay. Receipt
lookups also reject a replaced source even when no receipt exists. A source reset
requires explicit reconciliation; pending writes are never silently replayed into
new storage. Server [receipt compaction](operations-visibility.md) returns terminal
HTTP 410 `receipt_expired`. The HTTP owner rejects with DatabaseError code receipt_expired and
details.committed set to true. It stops checking and never treats expiration as a
missing receipt or proof of a failed mutation.

A `round` SSE event has `{subscription, sequence, round}` with event ID
`subscription:sequence`. The round contains the head fields and `results`, with one
`{key, result}` or `{key, error: {code}}` for every captured query. Membership must be
nonempty, have unique nonempty keys, and contain at most 128 queries. The client
checks contiguous sequences starting at one, exact membership, identity, source,
revision and all result shapes before publishing anything. Each evaluation uses one
read-only repeatable-read transaction, with savepoints isolating ordinary member SQL
failures. Infrastructure failures discard the whole round. Within one host, streams
with the same source, scope, principal, authorization scope and captured query set
share a refresh. Each connection retains its own credential checks,
delivery sequence, capacity permit and lifecycle. Credential checks run independently
of database polling, including before the first queued snapshot is delivered.

Each poll checks the source and scope revision. When they have not changed, the
runtime reuses the previous round, including member errors; any scope write triggers
full evaluation and authorization inside the shared snapshot. Empty reads, failed
queries and permission changes remain covered
by this conservative scope-wide invalidation. Watched handlers must derive results
from managed reads, fixed identity and input; clocks, randomness and external data
are not tracked dependencies. Direct query calls always evaluate the handler.
Joining a group also rechecks policy immediately, while retaining the group's
existing poll deadline.

Refreshes follow consumer demand and coalesce concurrent readers. Dropping the last
stream releases its cached round, and failures/cancellation invalidate reusable
coverage. This coordination is in-process; separate hosts poll independently.
Whole-table dependencies are recorded but do not yet filter unrelated scope writes.
Heartbeats keep the connection active. Each reconnect starts a fresh
subscription with a complete replacement round; Last-Event-ID does not establish
retained delta replay or local row coverage.

## In-memory results, confirmation and ownership

The TypeScript HTTP adapter submits a mutation exactly once. Interrupted confirmation
enters recovering on the original observable handle and checks the authorized receipt;
null means keep checking, never resend the payload. Submission and all checks share
one confirmation deadline (30 seconds by default, configurable confirmationTimeoutMs).
Further connectivity failures do not restart the deadline. Deadline expiry is a
DatabaseError confirmation_timeout, not evidence of rollback. Verified commits with
unusable results preserve committed evidence in the error details.

Query/mutation calls return immediately-started awaitable handles; public query
observation uses subscribe. Query states are pending/success/error, with readonly
results and isStale/isOptimistic indicators. Mutations additionally expose recovering.
Repeated awaits reuse the same outcome. Individual query observers can request
keepPreviousDataOnError; access and identity failures override retention.

All actively observed exact function/input queries in one TypeScript runtime share
one stream. Membership changes replace the stream and fence its old responses.
The SDK installs every result and optimistic base before notifying application
listeners, including listeners reading sibling query handles. One-shot responses
resolve their own awaits without replacing observed results. Incomplete frames
cannot publish, and incoming rounds cannot regress revisions. Ordinary member
errors remain subscribed and recover after scope writes; access errors clear the
affected result even under stale retention. Application auth subscriptions call generated `setAuth` with
`{ key, expiresAt? }`, or `null` on expiry/sign-out. A promise resolving to either
infers loading and immediately retires the previous scope. Await it before issuing
calls that depend on the resolved identity. Only the latest update applies; rejected
resolution blocks calls until another update. A changed state retires old calls,
streams, completed handles and optimistic layers before notifying observers. Late
replies cannot repopulate that state. Reattach subscriptions for the new identity.
Credentials remain separate; client metadata cannot grant access.

Live delivery revalidates through the provider using the stream's original headers.
If those credentials expire after the provider has refreshed them, the client can
reconnect with current credentials only after verifying the same context. It does
not implement provider refresh, switch identities silently, or keep protected data
when verification fails. Repeating an unchanged synchronous client identity preserves results.
An authenticated `setAuth` object can optionally provide `expiresAt` in Unix
milliseconds to retire local state at that deadline. It is not required and does
not change server credential verification. Refreshing the deadline for the same
identity preserves valid results; omitting it removes the timer. Expired handles
remain retired even if the same identity subsequently signs in again.

There is no browser persistence, pending-intent journal or recoverPending API.
Disposal stops local work; reload fetches fresh server state. Neither operation
cancels a submitted server transaction. Automatic confirmation is bounded to the
lifetime of the original in-memory handle.

Use mutation.withOptimistic(...targets) for pure immutable query-cache updates.
Missing results are skipped; failures remove that invocation's layers without
preventing server execution. Interruption removes prediction immediately. See
[optimistic results](concurrency-and-optimism.md) for visibility and rebase behavior.

The service durably records admission, success and failure. Lookup confirms original
delivery under verified caller/scope/source fences, without replaying application
policy. Reusable tickets renew only for future calls; automatic cleanup retains
accepted windows and fences duplicates after result compaction. Pre-admission
capacity errors are transient; a persisted `quota_exceeded` error is terminal.
Coordinated query publication includes [stable live pagination](live-pagination.md)
within one TypeScript runtime. Native Rust subscriptions use
singleton rounds, so separate native subscriptions do not share a snapshot guarantee.

Direct server setup now uses `defineConfig`, generated `_generated/server` and the
`generate`/`init` commands. See [server database access](server-database.md).

## Validation

```sh
make sdk-check
make sdk-adapters-test
cargo test -p hulladb_postgres --test managed_functions --locked -- --ignored
```

SDK tests inject dropped acknowledgements, disconnects, malformed data, source
replacement, permission revocation and identity retirement during acknowledgement.
The deployed test runs generated bindings against an ephemeral host and an owned
PostgreSQL schema, including constraint-failure rollback and lost-response recovery.
Requires Bun and the development PostgreSQL service; successful runs clean up their
owned schemas.

For real browser verification, run `bun tests/browser-check-server.ts` in
`packages/db` and open the printed loopback URL. The page checks automatic receipt
confirmation, no payload replay, deadline errors, int64 and identity cleanup. Navigate
to `?reload` on the same origin and reload the page to verify fresh authoritative
reads. The loopback fixture server retains its rows; browser storage is not involved.
The separate deployed test uses the actual PostgreSQL HTTP/SSE host.
`HULLADB_BROWSER_CHECK_PORT` can fix the harness port for repeated navigation.
