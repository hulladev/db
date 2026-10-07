# Authenticated managed task host

`hulladb_server::TaskHost` hosts the fixed task command/query slice over HTTP JSON
and server-sent events. It owns periodic durable-revision polling, an independent
polling connection, bounded request admission and a bounded command/delivery
connection pool. It does not install schemas, provision roles or reset existing data.
The embedding application supplies an `Authenticator` and serves `host.router()`.

## Identity and authorization

Every endpoint requires one `Authorization: Bearer <credential>` header. The
trusted authenticator returns a principal/workspace `Session`; bodies cannot choose
those identities. Unknown body and command fields are rejected. PostgreSQL checks
current workspace membership for commands, receipt recovery, snapshots and dequeue.
A token grants no direct database access. Read-only members cannot mutate; receipts
remain scoped to the original principal as well as workspace/request ID.

Implement issuer, signature, audience, expiry and revocation checks in the application
provider when using signed tokens, or resolve opaque credentials through a trusted
store. Provider failures fail closed. Streams reauthenticate on credential-check
intervals and before releasing data, including after database waits. A token that
now resolves to another identity closes the old stream; it never rebinds its query.
No token, SQL error or database connection string enters public error responses.

The example uses one configured opaque development credential and an already
initialized workspace/member. It binds HTTP to `127.0.0.1:8787`. The embedding
application owns HTTPS termination, its identity provider and origin policy.
Native PostgreSQL TLS remains unimplemented; the current store uses `NoTls`.

## Endpoints

| Method and path | Request | Response |
| --- | --- | --- |
| `POST /tasks/commands` | `CommandRequest`: request ID and create/patch/delete command | Committed `Receipt`, including its original source and workspace revision |
| `GET /tasks/receipts/{request_id}` | Authenticated identity and stable request ID | Original committed receipt, or JSON `null` when none is currently recorded |
| `POST /tasks/subscriptions` | Exact `TaskQuery`: completed filter, exclusive UUID bound and limit | SSE stream of complete scoped results |

For example, a command body has no session field:

```json
{
  "request_id": "019c1234-5678-7000-8000-000000000001",
  "command": {
    "Create": {
      "id": "019c1234-5678-7000-8000-000000000002",
      "title": "First task",
      "completed": false
    }
  }
}
```

A command responds only after commit, without awaiting polling, transfer or client
publication. Patch/delete use the existing expected row version; workspace locking
and stale-write rejection remain the authoritative concurrency policy. Identical
request retries recover one effect. Reusing an ID with changed arguments conflicts.
Portable command/receipt/error contracts live in `hulladb_protocol`; clients need no
PostgreSQL driver to deserialize them. PostgreSQL reexports its existing type names.

## Subscription lifecycle

Each `delivery` event contains one JSON `TaskMessage`. Its session/query identifies
coverage, its cursor orders replacements, and its visibility marker is the revision
paired with those rows. The first message establishes the subscription ID; register
it explicitly in `TaskReplica` before applying data. `Revoked` and `ResetRequired`
messages are terminal. Membership revocation can erase a queued snapshot before it
is released. Credential failures instead produce an `error` event and close.

Comments are heartbeat frames and carry no freshness guarantee. Error events use
`ApiError`, for example `{"code":"unavailable"}`. EOF, an error or connection loss
must transition the receiving SDK out of its connected/current state. Previously
published SQLite coverage is historical until a fresh authenticated lifecycle is
established. Credential/account changes must invalidate the old identity lifecycle.
Already observed network bytes cannot be recalled by later revocation.

Use a fetch/streaming HTTP client for the authenticated POST response; native browser
`EventSource` alone does not supply this request contract. The internal host sends
one complete bounded page per event, rather than streamed row chunks. Slow consumers
retain one latest result through the underlying watch feed; intermediate revisions
can coalesce. HTTP/kernel buffers may still hold bytes already emitted.

Reconnect by making a new authenticated subscription request and registering its
returned lifecycle. This slice starts from a fresh authoritative snapshot; it does
not replay `Last-Event-ID`. Feed sharing is scoped to identical principal, workspace
and exact query. Independently delivered queries are not one atomic client snapshot.
Receipt-to-query visibility uses `Receipt::visibility_fence()` and
`TaskReplica::is_visible`; receipt payloads are never applied as delivery events.

## Polling, failure and shutdown

The worker polls durable PostgreSQL revisions at the selected cadence, regardless
of command wake-ups. Successful host commands also wake it as a latency optimization.
Managed writes on another connection are detected without a notification. Polling
reads permissions/revisions and snapshot rows through the existing managed feed;
there is no replication connection or full server row mirror.

A failed/timed-out poll invalidates old streams and temporarily rejects requests.
The worker reconnects on later ticks and binds a fresh feed to the current source.
Restored availability cannot reopen an old stream, even if that stream skipped the
unavailable interval. New subscriptions bootstrap current authoritative state.
The pool discards closed connections and replaces them before reuse.

`HostOptions` explicitly selects polling and credential-check intervals, operation
deadline, pooled database connections, concurrent request admission, subscription
capacity and encoded request-body budget. One extra connection belongs to polling.
Idle subscriptions wait without leasing a connection. Request admission ends after
HTTP response creation; live SSE capacity uses a separate permit held across the
entire stream lifetime, including unread streams from a failed/recovered feed. The
fixed slice retains its existing 1–128 subscription limit. Query cardinality remains
the fixed 1–100 contract. These are internal slice policies, not production capacity claims. The host bounds an entire
poll operation, so a persistently overloaded feed fails closed rather than claiming
freshness. Poll cadence excludes query, transfer and client publication costs.

A mutation deadline returns `outcome_unknown`, since cancellation may race commit.
Recover or retry using the original request ID. A `null` recovery response is not
proof that an earlier request still running elsewhere can never commit. Authorization
failures are 401/403, input failures 400/413/415/422, conflicts 409, exhausted admission
429 and unavailable/ambiguous outcomes 503. Public bodies expose only structured codes.

Call `host.stop()` before HTTP graceful shutdown so idle streams close and new
requests fail. It closes idle pooled connections and cancels pool waiters; already
leased commands may complete. `host.shutdown().await` also joins/cancels the polling
worker and closes its connection. Dropping the owner stops/aborts its worker. Retained
routers cannot restart a stopped host. Use receipt recovery for interrupted commands.

## Development and validation

Create the current development schema and a workspace/member explicitly through the
existing trusted PostgreSQL probe. Export `HULLADB_DATABASE_URL`,
`HULLADB_DEV_WORKSPACE`, `HULLADB_DEV_PRINCIPAL` and `HULLADB_DEV_BEARER` for that
fixture, then run:

```sh
cargo run -p hulladb_server --example task_server --locked
```

For an initialized fixture and local host, a subscription can be inspected with:

```sh
curl --no-buffer --request POST http://127.0.0.1:8787/tasks/subscriptions \
  --header "Authorization: Bearer $HULLADB_DEV_BEARER" \
  --header 'Content-Type: application/json' \
  --data '{"completed":null,"after":null,"limit":100}'
```

```sh
make server-test
cargo clippy -p hulladb_server --all-targets --locked -- -D warnings
```

The ignored PostgreSQL tests cover real loopback HTTP/SSE, malformed/forged scopes,
read-only membership, duplicate submission and receipt isolation, unhinted polling,
network delivery through client SQLite and visibility, idle credential changes,
queued revocation, slow-stream capacity release, disconnect/reconnect, bounded
request admission, timeout recovery, shutdown with retained routers and unread old
streams that skip the unavailable interval without regaining data or freeing capacity. They reuse
managed fixtures and terminate only their own application-named connections.

The development example compiles with the same public host/authentication APIs.
SDK reconnect/account-switch handling, browser persistence, native database TLS,
production role provisioning and general application handlers remain future work.
See [managed synchronization](managed-sync.md) and [client persistence](client-replica.md).
