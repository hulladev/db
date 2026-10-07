# Diagnostic PostgreSQL CDC tail and event storage

`hulladb_cdc` is a retained diagnostic capture implementation, not the managed sync path. PostgreSQL business tables remain
authoritative. It consumes actual committed row changes through `pgoutput` v1 and
stages encoded changes in bounded SQLite chunks and publishes complete transactions
in a separate SQLite file. It creates no
per-mutation event tables in the source database and requires no workspace-wide
sequence lock on writers.

This is a **tail-only capture boundary**. `Capture::create` starts a dedicated
slot at its consistent point without copying existing rows. The production
[managed feed](managed-sync.md) does not use this tail, a publication, or a slot.
The raw [snapshot and replica](cdc-snapshot.md) is an optional diagnostic feature.
Cursor zero in the event log means the start of this tail, not a complete snapshot
of the database. Never expose this raw log to clients: it contains unauthorised
source rows, while the task feed returns scoped projections.

## Run

```sh
make db-up
make cdc-test
make cdc-bench
make cdc-bench-compare
```

The development Compose service enables `wal_level=logical`, with a 256 MB
`max_slot_wal_keep_size` limit. Updating an existing container restarts it while
preserving its volume. The WAL bound is enforced at PostgreSQL checkpoints and
can invalidate a lagging slot; it is not an exact instantaneous disk cap.

Tests and benchmarks use the usual `HULLADB_TEST_URL` override, defaulting to
`postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev`. They create UUID-named schemas,
publications, durable logical slots, and temporary SQLite files. Successful runs
drop only their own PostgreSQL fixtures and remove their local files. A failed
run may leave its PostgreSQL fixture/slot for diagnosis; inspect and remove only
those owned fixture resources. Never drop the user's database or recreate an
existing slot to mask an error.

The probe assumes development privileges, including `pg_control_system()` and
logical slot management. Production privilege handling, TLS, pooling, and
supported PostgreSQL provider/version combinations are not established.

## Capture and durability

1. A dedicated SQL connection acquires cooperative advisory ownership of its
   slot and verifies cluster/database/publication identity plus UTF8/logical WAL.
   The replication connection uses the same single host, port, database, user and
   deterministic text-output settings. Multi-host/failover and TLS are not supported
   by this development boundary.
2. On first receive or reconnect, capture verifies slot availability and checks
   that its acknowledgement is not ahead of durable local state. It starts a
   persistent `START_REPLICATION ... LOGICAL` connection at the upstream
   acknowledgement. The SQL peek/advance implementation is removed.
3. `Capture::next()` awaits one complete transaction or idle decoding checkpoint.
   The pgwire transport keeps a 32-event queue and sends status feedback every
   100 ms, including while its queue is full. This is an event-count bound, not a
   strict byte bound. Startup occurs on first receive and belongs in a warmup when
   measuring steady-state latency.
4. The decoder assembles BEGIN, relation metadata, row changes, and COMMIT.
   INSERT, UPDATE, DELETE, and TRUNCATE retain old keys, nulls and unchanged-TOAST
   markers. Type OIDs accompany text values; typed conversion remains separate.
   Logical messages are ignored with byte accounting; they are not domain events.
   Message-only transactions advance progress without creating row-delivery batches.
5. The decoder emits individual changes, without an accumulating transaction Vec.
   JSON is written through a bounded buffer into hidden durable SQLite chunks.
   At COMMIT, one WAL/FULL transaction publishes their sequence, advances the
   checkpoint and trims complete history. A transaction fitting one chunk writes
   its body and publication together, avoiding extra staging commits. Opening
   storage verifies durability modes and rejects volatile in-memory databases.
6. Only after that commit does capture report its checkpoint to the replication
   worker. Upstream feedback is asynchronous; visibility does not wait for it.
   XLogData's advertised server WAL end is never used as a durable acknowledgement.
   Ordered PostgreSQL logical keepalives outside an in-flight decoded transaction
   record idle scan progress without creating delivery batches. This permits quiet
   publications to release unrelated WAL while preserving transactions that commit
   later; PostgreSQL still retains their required restart WAL.

Cancellation while awaiting input retains the decoder, connection and staging. Decode,
transport or storage failure aborts the stream and discards partial decoding and
unpublished staging; the next receive rechecks the slot and replays from its
acknowledgement. A crash
may leave hidden chunks; the next ingestion owner removes them before replay.
Opening a log for inspection does not remove another owner's active staging. Complete
batches already persisted deduplicate by their commit boundary, including batches
consumed from the queue before a disconnect surfaces. No partial transaction is
published or acknowledged. `persist_next()` leaves feedback unchanged to exercise
the durable-before-feedback crash window. Graceful `close()` waits for feedback
already reported by `next()` and for slot inactivity before releasing ownership;
dropping capture stops both connections without advancing feedback.

Advisory ownership coordinates these collectors only. Another application or
administrator must not consume, advance, drop, or recreate the dedicated slot.
The caller owns a stable source identity. Source resets and publication changes
require explicit reconfiguration/bootstrap; this probe has no automatic repair.

## Bounded history and growth

Default retention is at most **128 complete batches and 16 MiB of encoded payload**
after each append. Retention runs in the same transaction as append/checkpoint.
The persisted floor makes an expired raw-log cursor return `CursorExpired`.
The task feed uses the current head for invalidation and reads fresh authorized
PostgreSQL results, so it does not require retained raw history. Client lag
does not extend history retention or delay upstream acknowledgement.

The former 4 MiB/10,000-change transaction ceilings are removed. `StorageOptions`
controls the encoded chunk buffer (default 256 KiB), SQLite busy timeout and an
optional `max_staged_payload_bytes` quota. `EventLog::open_with_options` lets a
host select these budgets; defaults impose no total transaction-size ceiling.
The buffer may split JSON or UTF-8 across chunks. Readers reconstruct the stream,
not a complete encoded transaction. A decoded change and relation cache still
consume memory independently; this is not a total process RSS guarantee.

The staging quota is an operator disk-pressure policy, independent of history
retention. Exceeding it returns `StagingBudget` without publication/feedback.
Disk exhaustion behaves the same way; recovery requires freeing space or changing
that configured policy. The quota measures encoded change bytes, not database/WAL
file size or filesystem capacity.

A transaction larger than the retained history byte budget is published and pruned
whole in the same checkpoint transaction. Capture and PostgreSQL acknowledgement
continue; an older raw-log reader gets `CursorExpired`. The authorized task feed
requeries PostgreSQL and does not require retained row history. Diagnostic mirrors
must rebootstrap after expiry. Staging can therefore temporarily exceed retained
history size; disk provisioning/checkpoint maintenance remain operational work.

`visit_transactions` / `StoredTransaction::for_each_change` provide streaming
reads inside one consistent SQLite snapshot. Consumers applying derived rows must
keep their own write transaction open until the stream completes successfully;
decoding/storage errors can occur after a prefix. Diagnostic replica application uses
these reads and publishes its rows/cursor atomically. `since` remains a convenience
API collecting results into a Vec, with a default whole-page budget of 4 MiB and
10,000 changes. `since_with_budget` makes that read budget explicit; `ReadBudget`
failures neither halt capture nor change its checkpoint. Encoded-byte budgets are
not exact heap limits.

The separate 1 MiB protocol-message and relation/column bounds remain. The transport
allocates wire frames before the decoder's check and has its own 1 GiB frame cap.
Its 32-event queue therefore does not establish a small maximum memory footprint.
Byte-limited transport allocation is the next resource-policy issue, not solved by
transaction staging. Ignored logical messages are validated individually but do
not accumulate transaction memory or consume change-history quota.

Log storage keeps transaction headers and ordered encoded-change chunks.
Incompatible development files must be recreated explicitly; opening a file does
not upgrade it. The payload counter counts encoded changes (including delimiters),
excluding headers. Pruning deletes whole transactions and
cascades their chunks; accounting/checkpoint failures roll back publication and
pruning together. Retained replay compares canonical bytes incrementally, including
when the configured chunk size changes between runs.

Deleting history bounds live payload but does not immediately shrink SQLite's
files. Freed pages can be reused, and SQLite's own WAL has a separate footprint.
Long-lived readers, WAL checkpointing, storage backups, and database-file
maintenance need additional operational coverage.
SQLite calls currently run synchronously in the receiving task. A production async
collector needs a dedicated storage worker and bounded queue so disk operations
cannot stall unrelated service work.

## Verification and measurements

Database tests cover external SQL writes, transaction grouping, rollback
exclusion, actual trigger results, concurrent independent writers, local
failure-before-checkpoint, durable-before-feedback restart, cooperative ownership,
quiet-publication progress with a long-running transaction, expired cursors,
slot loss/ahead detection, stale replacement-log rejection, large transactions
beyond both former ceilings, durable-before-feedback replay
with changed chunk sizes, cancelled waits,
buffered commits across disconnect, ignored logical messages, and a full event
queue paused beyond a 500 ms sender timeout without premature acknowledgement.
Pure tests cover SQLite rollback/reopen/deduplication, rejection of volatile storage,
count/byte retention, atomic accounting and pruning rollback, cursor scope,
LSN roundtrips, truncated protocol frames,
individual ignored-message validation, hidden staging/restart, quota failures,
streamed large reads, and unsupported tuple formats.

The current six-case suite uses five warmups and 31 measured requests, recording
median/MAD, raw samples and coarse p95. The retained
`benchmarks/baselines/cdc-snapshot-local.json` baseline predates chunked storage;
fixture version four requires an explicit new baseline. Startup, fixture setup and cleanup
are excluded after warmup. Capture measures persistent streaming reception and
durable local append, without waiting for asynchronous PostgreSQL feedback.
Assertions verify complete transactions rather than timing empty polls. See [methodology](benchmarks.md)
and an actual-command capture diagnostic. Managed visibility is tested separately
in `hulladb_sync`, without WAL capture.

An earlier storage growth fixture sampled 64, 128, 256, and 512 one-row
commits. At 256 and 512,
live history stayed at 128 batches / 57,344 payload bytes. SQLite page footprint
stayed at 94,208 bytes, with 8,192 reusable bytes; its WAL file was roughly 4 MiB.
The ordinary PostgreSQL business table grew as records were inserted. No source
event/receipt table was created by this capture path. These small fixed fixtures
are historical evidence, not measurements of the current chunked storage or a general
storage-amplification ratio. Current chunked workloads require fresh baselines.

## Next boundary

Native authorized query bootstrap and durable local application are implemented
in the [managed task feed](managed-sync.md) and [client replica](client-replica.md).
Production work still includes authenticated network delivery, browser storage,
typed/schema evolution handling and mutation visibility correlation. Raw MVCC
copying remains [diagnostic](cdc-snapshot.md), with no application XID epoch
restriction.
Configurable backend concurrency policies remain separate work. Diagnostic CDC
observes commits without enforcing application conflict rules. This crate is not
a supported external-writer connector or a production serving dependency.

References: PostgreSQL [logical decoding and snapshot concepts](https://www.postgresql.org/docs/18/logicaldecoding-explanation.html),
[logical replication messages](https://www.postgresql.org/docs/18/protocol-logicalrep-message-formats.html),
[logical decoding SQL functions](https://www.postgresql.org/docs/18/functions-admin.html#FUNCTIONS-REPLICATION),
and [replication resource limits](https://www.postgresql.org/docs/18/runtime-config-replication.html).
