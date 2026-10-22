# Transactional outbox

**Implementation status:** this page describes the existing outbox, including its
automatic delivery retries. The action architecture selected on 12 October 2026
requires no automatic action retries by default, including scheduled executions;
retries require explicit opt-in. Worker recovery must not repeat a possibly started
action without that opt-in. The existing defaults and recovery behavior below need
to be updated during implementation; they do not describe the new action contract.
See [action retry policy](backend-functions.md#commit-rollback-and-retry-examples).

Mutations can enqueue external work in the same PostgreSQL transaction as their
rows and recovery receipt. A separate application-owned worker delivers committed
jobs. A rollback, failed result validation or failed quota check leaves no job;
receipt recovery never executes the handler or enqueues it again. Serializable
retries discard the failed attempt's jobs before retrying the whole mutation.

Delivery is at least once, with bounded attempts and retained dead letters. A
service may accept a request before its acknowledgement is lost, a handler times
out or a worker crashes. Use the stable job UUID as the external service's
idempotency key. Cancellation or a lease fence cannot undo an external effect.

## Enqueue from a mutation

The typed Rust facade accepts any `Codec` payload. Its canonical JSON encoding
preserves int64 values as decimal strings:

```rust
let entry = ctx.db.insert(EntryInsert { title }).await?;
let job = ctx.db.enqueue("entries.created", entry).await?;
```

`hulladb_postgres::managed::WriteDb` accepts a `serde_json::Value` payload.
`enqueue_with(name, payload, EnqueueOptions { max_attempts, delay })` selects an
attempt limit and initial delay. Defaults are ten attempts and no delay.

TypeScript callbacks use the same native transaction:

```ts
const row = await ctx.db.insert("entries", { title, owner: ctx.identity });
const job = await ctx.db.enqueue("entries.created", {
  id: row.row.id,
  ordinal: row.row.ordinal.toString(),
}, { maxAttempts: 10, delayMs: 0 });
```

Payloads are JSON. Encode bigint values explicitly as strings; undefined, unsafe
integers, nonfinite numbers, sparse arrays, invalid Unicode and non-JSON objects
are rejected by the TypeScript adapter. All database operations, including
`enqueue`, must be awaited. Query contexts have no enqueue capability, and the
native worker bridge rejects forged enqueue operations from queries.

Names contain 1..128 ASCII letters, digits, dots, hyphens or underscores. One
mutation may enqueue at most 100 jobs and 1 MiB of combined serialized payloads
and names, including jobs created by custom key generators. Attempts are 1..100;
initial and retry delays are bounded to seven days, with millisecond precision.
These limits apply even without a scope quota. Applications should construct job
names and payloads under their registered mutation's authorization policy.

Authorization approves the intent at enqueue time. Workers do not impersonate the
original principal or reuse its credentials. If delivery requires a fresh access
check, implement it in the handler through an authorized application service.

Each job records its UUID, source fence, scope, principal, authorization scope,
request UUID and originating operation. Jobs are private operational state:
job-only mutations receive receipts without advancing query-visible revisions.
A mutation receipt confirms durable enqueueing; it does not confirm delivery.
Explicit [receipt compaction](operations-visibility.md) leaves jobs untouched and
keeps request markers so an expired receipt cannot enqueue another job on retry.

## Worker ownership and delivery

Implement `OutboxHandler::deliver(&OutboxJob) -> DeliveryFuture` in the trusted
worker process. Dispatch on `job.name`, decode and validate `job.payload`, and
pass `job.id` to the external service for deduplication. Keep service credentials
in this process. The TypeScript transaction worker receives none of those secrets.
All workers consuming a namespace must understand its job names; an unsupported
name can return `DeliveryError::Permanent` and remain available for operator repair.

Connect `OutboxWorker::connect(url, schema, expected_source, WorkerOptions)` using
an explicitly configured source and dedicated worker login. It verifies existing
storage and source identity without installing anything. `deliver_next(&handler)`
claims one job, commits the lease, calls the handler outside any database
transaction, then acknowledges or schedules a retry. It returns `Idle`,
`Delivered(id)`, `RetryScheduled(id)` or `DeadLettered(id)`. The embedding process
owns its polling interval, shutdown, logging and handler implementation; constructing
or dropping a host does not start a delivery worker.

Default `WorkerOptions` use a 30-second lease, a ten-second delivery timeout and
a five-second retry delay. The timeout must be positive and shorter than the lease;
leases are bounded to one hour. A retryable error or timeout reschedules the job
until its stored attempt budget is exhausted. A permanent error dead-letters it
immediately. Last-error diagnostics are limited to 1024 UTF-8 bytes; redact service
responses and credentials before returning an error.

Workers claim through PostgreSQL `FOR UPDATE SKIP LOCKED`. A crash or cancelled
future leaves its committed lease to expire. Reclaiming the same job gives it a
new private token and increments its attempt count. Old or expired tokens cannot
acknowledge, renew or reschedule it. A crash on the final attempt becomes a dead
letter during subsequent claim maintenance. Claims and expiry maintenance are
bounded to 100 rows per call; there is no strict global FIFO ordering.

Advanced dispatchers can use `ManagedStore::claim_outbox(expected_source,
ClaimOptions)`, `renew_outbox`, `acknowledge_outbox` and `retry_outbox` directly.
Acknowledge deletes the successfully delivered job. When claiming batches, account
for every job's lease while executing them; the built-in worker claims one at a
time to avoid expiring later jobs during sequential delivery. Database failures,
including an unknown claim/acknowledgement commit outcome, are returned to the
caller; lease expiry and external idempotency provide recovery.

## Roles, quotas and reconciliation

`Roles` separates owner, runtime, backup and worker groups. Runtime connections
can insert jobs and read usage but cannot claim, acknowledge or reset them. Worker
connections can read deployment metadata and jobs, update delivery bookkeeping
and delete acknowledged jobs; they cannot read/write application tables, replace
payloads or rotate sources. Operator retry/discard APIs require schema operator
privileges. These are trusted service credentials; direct SQL outside these APIs
is unsupported. Backup connections can read the queue. Give each login only its
intended group via `grant_role`.

`ScopeQuota::max_outbox_jobs` and `max_outbox_bytes` bound retained work per scope.
Job counts include pending, leased, failed and old-source jobs. Bytes count the
stored PostgreSQL JSON text and name, excluding lease metadata, diagnostics and
indexes. Checks run before mutation commit, so a full queue rolls back its rows,
jobs and receipt together. Delivery deletes release capacity; dead letters stay
retained. `scope_usage` reports both counters.

An operator can call `inspect_outbox(scope, limit)` for bounded inspection.
`requeue_outbox(id, previous_source, current_source)` explicitly resets attempts
and delays while preserving the job UUID. It requires operator privileges and
rejects active leases. Repair the handler or external condition before retrying.
`discard_outbox(id, previous_source, current_source)` explicitly abandons a selected
job and releases its quota. It also requires operator privileges and rejects
active leases; there is no automatic dead-letter deletion.

Source rotation and backup restoration preserve queued jobs with their original
source. Current-source workers do not claim them; old-source workers cannot claim,
acknowledge, renew or retry after rotation. Stop old workers, wait for leases to
expire and reconcile selected jobs explicitly with `requeue_outbox`. Preserve the
same idempotency key at the external service across restore. Source checks cannot
recall an external request already in flight.

This adds managed storage tables and quota columns. Existing development layouts
are rejected with `SchemaMismatch`; recreate development fixtures explicitly.
There are no automatic upgrades, resets or schema generations. The self-hosted
initializer creates the worker group; optionally set `HULLADB_WORKER_LOGIN` to
grant an existing dedicated login. The starter does not launch application workers.

## Validation

```sh
make outbox-test
cargo test -p hulladb_postgres --test production_operations --locked -- --include-ignored
make typescript-functions-test
```

These require the development PostgreSQL instance; TypeScript execution also
requires Bun. Tests cover commit visibility, rollback, duplicate requests,
serializable retries, quotas, competing workers, stale leases, final-attempt
crashes, delivery timeouts/cancellation, source reconciliation and role separation.
The backup integration verifies that restored jobs retain their IDs, payloads and
old source while the deployment gets a new source.
