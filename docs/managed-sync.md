# Managed transactions and authorized synchronization

Decision, 6 October 2026: HullaDB manages application writes. PostgreSQL remains
business source of truth; clients persist authorized results in SQLite. Durable
transaction revisions drive synchronization. WAL capture is not required by the
managed path. `hulladb_sync` owns the internal fixed task serving adapter and does
not depend on `hulladb_cdc` or `pgwire-replication`.

## Correctness contract

| Invariant | Implementation and recovery |
| --- | --- |
| Valid committed state | PostgreSQL constraints, authoritative permission checks and operation concurrency protection remain mandatory. Synchronization cannot repair an invalid transaction. |
| Atomic tracking | Task changes, workspace revision and request receipt commit together. Membership changes and their revision commit together. A tracking failure rolls back the write. |
| Concurrent commands | The fixed task slice locks its workspace before permission checks, version comparisons, mutation and receipt recovery. Duplicate submissions have one effect; stale edits conflict. The counter is protected by that lock, not a sequence allocated before commit. |
| Complete snapshot | Permission, source identity, revision and projected rows share a repeatable-read snapshot. A later commit cannot be silently marked covered by older rows. |
| Bootstrap handoff | The feed remembers the revision paired with the initial rows. Later polls compare against durable PostgreSQL state, including commits made while client bootstrap is unpublished. |
| Missed wake-up or restart | Revisions survive process failure. Periodic polls recover missed work. A restarted feed creates fresh delivery lifecycles and authoritative snapshots. |
| Revocation | Permission changes advance the revision. Refresh and immediate dequeue authorization close revoked lifecycles; regrant cannot reopen an already closed lifecycle. |
| Client publication | SQLite publishes query rows, retained delivery history, delivery cursor and source/revision visibility progress atomically. Failed bootstrap keeps prior coverage. |
| Source reset | Deployment UUID changes or revision regression terminate affected subscriptions with `ResetRequired`. A new authenticated subscription has a new subscription ID. |

The fixed task backend implements only create, version-checked patch/delete and
trusted membership administration. It is not a general arbitrary-handler runtime.
General multi-row operations must demonstrate serializable behavior or equivalent
locking for their invariants before being supported. Row versions do not prevent
write skew across different rows. Cross-workspace atomic operations, automatic
transaction retries and configurable operation concurrency remain future work.
Keep current workspace serialization until narrower coordination is proven correct;
its contention cost is a separate audit issue, not a reason to weaken integrity.

## Write boundary and administration

Application mutations must use managed operations. Raw SQL, unmanaged triggers,
cascades into independently tracked scopes and independent application writers are
not supported write paths. Future operation runtimes must register all affected
query/policy dependencies; they must not guess them from a returned entity.

The current connection/schema helpers are development probes, using trusted server
credentials. They do not provision production roles or expose a public raw-SQL API.
Production deployments must separate schema ownership from the runtime role,
keep runtime credentials server-only, and grant no application/client role direct
DML or schema-change access to managed tables. The trusted service role needs only
its schema/table privileges, not replication, publication or control-system access.
The runtime role reads metadata; only the owner may rotate the source identity.
Protect the database boundary as well as the network mutation API. Administrator
credentials can bypass invariants; arbitrary external SQL is not promised fresh.

Before Cargo/npm publication, incompatible development schemas and SQLite fixtures
are recreated explicitly. There are no schema/file-format migrations or message
version negotiation. Do not automatically reset existing data on initialization.
An administrative restore, source replacement or out-of-band mutation requires a
controlled maintenance/reset: drain serving/writes, perform and validate the change,
rotate the metadata source UUID, then reopen the service and rebuild subscriptions.
Backups copy the UUID, so physical connection identity alone cannot detect a restore.
Existing client rows are historical until a fresh authorized lifecycle establishes
coverage. Do not rewind counters and silently reuse old subscription IDs.

## Query maintenance and delivery

`TaskFeed::new(store, max_subscriptions)` binds a feed to the initialized deployment.
`subscribe(store, session, query)` reads an authorized bounded snapshot. Identical
principal/workspace/query subscriptions share one Arc/watch result. PostgreSQL
executes the task query with workspace/key and status indexes. Only `id/title/completed`
are selected; no full server row mirror or server raw-event database participates.

The host MUST call `refresh(store)` on periodic ticks and after reconnect.
[`hulladb_server::TaskHost`](task-host.md) now owns this scheduler, connection
recovery and authenticated HTTP/SSE for the fixed slice. Calling
it immediately after a successful command reduces delay but is not recovery.
The `hulladb_sync` library itself contains no automatic scheduler or network service.
A future `LISTEN/NOTIFY` integration may wake the same polling loop; notifications
must never be its durable checkpoint. Poll cadence contributes to discovery delay and is
a caller-selected host policy; reevaluation, transfer and client publication add
their own latency.

Revision checks and snapshot reads can overlap commits. Refresh records the revision
returned with the replacement rows, not the earlier polling observation. An advanced
revision is delivered even when query rows are unchanged, so clients
can establish receipt visibility for filtered or top-N exclusions. An unchanged
revision/result is not resent. The current bounded adapter resends complete pages;
a lighter progress frame would need to preserve slow-consumer bootstrap guarantees.
Latest full replacements refill top-N membership and permit
slow receivers to skip intermediate states. Revisions record invalidation, not a
replayable audit trail of every application transition. Durable domain events or
external jobs need their own transactional outbox contract if introduced.

The workspace revision is a conservative dependency baseline: unrelated task or
membership changes can cause reevaluation. Dependency tracking and policy batching
remain later optimization work. The prototype query limit remains 1–100 and
subscription capacity 1–128; neither is an architectural correctness requirement.
Their admission/resource policies remain separate audit issues.

The portable envelope uses a `DeliveryCursor` with a subscription UUID
and monotonic replacement sequence. It differs from the managed source revision
and mutation receipt. Each query is individually consistent; independently delivered
queries/pages are not one globally atomic client snapshot. Commit acknowledgement
and client visibility remain distinct. Receipt-to-visibility fences are implemented
for this exact-query slice; optimistic reconciliation remains future work.

Dequeue checks current authoritative membership/source before releasing a pending
result. Cancellation retains that message for retry. A permission check does not
make network delivery atomic with concurrent revocation, and already observed data
cannot be recalled. Capture/transport failures must never imply current freshness.

## Receipt visibility

Each committed receipt stores its original deployment source UUID with its workspace
revision. `Receipt::visibility_fence()` produces a portable `VisibilityFence`.
Recovery and duplicate submissions return that original fence even after a source
reset; they never attach the current deployment identity to an old commit.

Each snapshot message carries the source/workspace/revision read with its rows.
Dequeue authorization must not advance this marker to a newer head: checking
permission does not reevaluate query data. SQLite publishes the marker only when
the complete replacement commits. `TaskReplica::is_visible(session, query, fence)`
checks persisted metadata without loading rows. It returns false when covered data
is behind the fence, and errors for absent/closed coverage or a foreign source.

Visibility means this exact query was evaluated at or beyond the commit. It does
not promise the row belongs to that query, has survived a later write, or that the
replica is current now. Each query/page needs its own check. Coalesced replacements
can establish several receipts at once. Reopening retains historical progress;
new lifecycle registration discards previous coverage, and revocation/reset closes
it. Source mismatch or revision regression within a lifecycle cannot advance progress.

## Validation

```sh
cargo test -p hulladb_postgres --test task_store --locked -- --ignored --test-threads=1
make sync-test
```

The managed tests create no publication, slot or server event-log file. They cover
real PostgreSQL commits and rollback, durable receipt recovery, client reopen,
missed wake-ups, commits during unpublished bootstrap, a commit interrupting a
snapshot read, policy invalidation without task changes, permission rollback when
revision tracking fails, top-N refill/coalescing, source reset, revision regression,
cancellation, a nonreplication runtime role, denied direct client SQL writes,
contradictory results at one revision, unchanged filtered/top-N receipt visibility,
coalesced visibility progress, recovered receipt source identity and authorized pages
over 100,000 source rows. These database tests are ignored in ordinary Cargo runs. Client SQLite failure paths have separate
coverage in `hulladb_client`.
