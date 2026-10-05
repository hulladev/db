# PostgreSQL CDC tail and separate event storage

`hulladb_cdc` is the selected internal capture implementation. PostgreSQL business tables remain
authoritative. It consumes actual committed row changes through `pgoutput` v1 and
persists complete transaction batches in a separate SQLite file. It creates no
per-mutation event tables in the source database and requires no workspace-wide
sequence lock on writers.

This is a **tail-only capture boundary**, not a complete sync service or serving
replica. This raw capture path does not copy existing rows. The separate
[snapshot and replica](cdc-snapshot.md) now provides a bounded MVCC copy
and transaction materialization. Cursor zero in the event log means the start of this tail,
not a complete snapshot of the database. Never expose this raw log to clients:
query coverage, field/row authorization, and transaction-aware result derivation
belong to the subsequent service layer.

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
5. One SQLite WAL/FULL transaction inserts a complete batch, updates checkpoint and
   delivery sequence, and trims history. Opening storage verifies those durability
   modes and rejects volatile in-memory databases.
6. Only after that commit does capture report its checkpoint to the replication
   worker. Upstream feedback is asynchronous; visibility does not wait for it.
   XLogData's advertised server WAL end is never used as a durable acknowledgement.
   Ordered PostgreSQL logical keepalives outside an in-flight decoded transaction
   record idle scan progress without creating delivery batches. This permits quiet
   publications to release unrelated WAL while preserving transactions that commit
   later; PostgreSQL still retains their required restart WAL.

Cancellation while awaiting input retains the decoder and connection. Decode,
transport or storage failure aborts the stream and discards partial decoding; the
next receive rechecks the slot and replays from its acknowledgement. Complete
batches already persisted deduplicate by their commit boundary, including batches
consumed from the queue before a disconnect surfaces. No partial transaction is
published or acknowledged. `persist_next()` leaves feedback unchanged to exercise
the durable-before-feedback crash window. Graceful `close()` waits for feedback
already reported by `next()` and for slot inactivity before releasing ownership;
dropping capture stops both connections without advancing feedback.

Advisory ownership coordinates these collectors only. Another application or
administrator must not consume, advance, drop, or recreate the dedicated slot.
The caller owns a stable source generation. Source resets and publication changes
require explicit reconfiguration/bootstrap; this probe has no automatic repair.

## Bounded history and growth

Default retention is at most **128 complete batches and 16 MiB of encoded payload**
after each append. Retention runs in the same transaction as append/checkpoint.
The persisted floor makes an expired cursor return `CursorExpired`, which the
future service must handle by delivering a fresh authorized snapshot. Client lag
does not extend history retention or delay upstream acknowledgement.

Limits are explicit: 1 MiB per protocol message, 4 MiB of accumulated/expanded
transaction data and encoded batch, 10,000 changes, and bounded relation/column
metadata. Exceeding a limit halts capture without publishing or acknowledging a
partial transaction. A large transaction can therefore block later capture;
production support needs a bounded spill/large-transaction strategy. The transport
allocates wire frames before our decoder applies its 1 MiB message limit, and its
own frame cap is 1 GiB. The 32-event queue therefore does not establish a small
maximum memory footprint. A byte-limited frame/queue transport and upstream
large-transaction handling remain production requirements.

JSON currently repeats relation metadata per change. The decoder charges that
expansion before cloning, and JSON serialization enforces its byte limit while
writing. This is an inspectable internal envelope, not a stable SDK/wire format
or an optimized representation. Retention maintains durable batch/encoded-byte
counters in the append transaction. Pruning reads only the removed payloads rather
than rescanning the retained history; rollback preserves rows, counters, cursor and
checkpoint together. Opening an older format-one log upgrades it atomically to
format two, counting its retained history once without changing source identity or
progress. Earlier collectors reject the new storage format.

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
slot loss/ahead detection, stale replacement-log rejection, oversized transaction
rejection, cancelled waits,
buffered commits across disconnect, ignored logical messages, and a full event
queue paused beyond a 500 ms sender timeout without premature acknowledgement.
Pure tests cover SQLite rollback/reopen/deduplication, rejection of volatile storage,
count/byte retention, atomic accounting upgrades and pruning rollback, cursor scope,
LSN roundtrips, truncated protocol frames,
ignored-message transaction limits, and unsupported tuple formats.

The current six-case suite uses five warmups and 31 measured requests, recording
median/MAD, raw samples and coarse p95. Its baseline is
`benchmarks/baselines/cdc-snapshot-local.json`. Startup, fixture setup and cleanup
are excluded after warmup. Capture measures persistent streaming reception and
durable local append, without waiting for asynchronous PostgreSQL feedback.
Assertions verify complete transactions rather than timing empty polls. See [methodology](benchmarks.md)
and the separate actual-command CDC suite for the commit/visibility boundary.

The growth fixture samples 64, 128, 256, and 512 one-row commits. At 256 and 512,
live history stayed at 128 batches / 57,344 payload bytes. SQLite page footprint
stayed at 90,112 bytes, with 8,192 reusable bytes; its WAL file was roughly 4 MiB.
The ordinary PostgreSQL business table grew as records were inserted. No source
event/receipt table was created by this capture path. These small fixed fixtures
establish a baseline, not a general storage-amplification ratio.

## Next boundary

An overlapping repeatable-read bootstrap and durable queryable replica are now
implemented in the [snapshot implementation](cdc-snapshot.md). Prove exported-snapshot
bootstrap through a replication connection, including arbitrary XID epochs. Add typed/schema evolution
handling, authorization-aware bounded query results, and mutation correlation.
Evaluate PostgreSQL versus SQLite sync storage with equivalent durability/workloads.
Configurable backend concurrency policies remain a separate mutation-execution task; CDC follows whatever commits
without changing application conflict rules.

References: PostgreSQL [logical decoding and snapshot concepts](https://www.postgresql.org/docs/18/logicaldecoding-explanation.html),
[logical replication messages](https://www.postgresql.org/docs/18/protocol-logicalrep-message-formats.html),
[logical decoding SQL functions](https://www.postgresql.org/docs/18/functions-admin.html#FUNCTIONS-REPLICATION),
and [replication resource limits](https://www.postgresql.org/docs/18/runtime-config-replication.html).
