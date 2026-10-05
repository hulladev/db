# Bounded CDC snapshot and durable replica

`hulladb_cdc` now copies existing published rows into a separate SQLite replica
and applies retained CDC transactions. PostgreSQL remains authoritative. This is
the internal full-publication materialization, not an authorized client snapshot, a
public SDK, or a general query-serving service.

## Snapshot handoff

Slots are currently created through SQL, separately from the streaming transport,
so bootstrap does not yet import a slot-exported snapshot. The implementation uses
an **overlapping repeatable-read snapshot**:

1. The caller creates a new dedicated durable `pgoutput` slot and records its
   identity and consistent start LSN in `Source`. Existing slots are never reset.
2. `SnapshotReader::begin()` acquires the collector's cooperative advisory lock
   and requires the slot to remain at its original acknowledgement position.
3. It acquires access-share locks on all explicitly published tables before
   acquiring the repeatable-read snapshot. Ordinary writes continue. TRUNCATE
   and conflicting DDL wait; PostgreSQL's TRUNCATE is not MVCC-safe against an
   older snapshot, so acquiring these locks before snapshot acquisition matters.
4. It records `pg_current_snapshot()` and a WAL insert boundary sampled after
   snapshot acquisition. Both are retained with the copied rows.
5. `read()` streams the supported columns using their native PostgreSQL text
   output. Collector and snapshot connections use the same output settings;
   booleans are `t`/`f`, including in initial copies. Null stays distinct from text.
6. `Replica::initialize()` installs all relations, rows, the visibility fence,
   source identity, and delivery cursor zero in one SQLite WAL/FULL transaction.
   Failed or oversized copies never return a complete `Snapshot`; failed replica
   initialization never publishes a partial ready state.
7. Capture resumes from the original slot start. For commits before the sampled
   WAL boundary, the replica checks the transaction's visibility in the saved
   snapshot: visible transactions are already copied, while transactions active
   at the snapshot or assigned afterward must be applied. Commits at or after
   the boundary are always applied. Thus a transaction can commit during the
   copy without creating a gap, and a commit present in both paths is not applied
   twice.

The snapshot uses top-level transaction IDs, matching pgoutput BEGIN messages.
PostgreSQL snapshot IDs are 64-bit whereas pgoutput transaction IDs are 32-bit.
This initial overlap implementation rejects bootstrap after the first XID wrap;
allocating/checking an XID after the WAL sample also rejects a wrap during fence
acquisition. Once commits pass the WAL fence, visibility is no longer consulted
and later wrap does not affect application. Exported-snapshot bootstrap through a
replication connection remains the preferred production follow-up, including
support for arbitrary XID epochs and eliminating the overlap filter.

See PostgreSQL's [snapshot visibility functions](https://www.postgresql.org/docs/18/functions-info.html#FUNCTIONS-PG-SNAPSHOT),
[exported logical snapshots](https://www.postgresql.org/docs/18/logicaldecoding-explanation.html#LOGICALDECODING-EXPORTED-SNAPSHOTS),
and [TRUNCATE caveats](https://www.postgresql.org/docs/18/sql-truncate.html).

## Materialization and recovery

Each retained transaction applies all its INSERT, UPDATE, DELETE, and TRUNCATE
changes and advances the replica cursor in one SQLite transaction. Primary-key
changes use the old key. Unchanged-TOAST markers merge with the already materialized
row; an absent prior row is an error. Duplicate inserts, missing deletes, changed
relation definitions, and malformed rows fail rather than silently repairing state.

Capture acknowledgement depends on durable event-log persistence, independently
of replica progress. A failure while applying changes or saving the replica cursor
rolls back the whole affected transaction. Reopening resumes from its old cursor
and retries from the retained log. A page may contain several transactions: earlier
successful transactions remain committed if a later one fails. Repeating catch-up
after success does not reapply rows.

`Replica::view(oid, limit)` couples its row page and cursor in one SQLite read
snapshot. Limits are 1–1,000 rows; `complete=false` explicitly distinguishes a
truncated result. This completeness flag describes one internal relation, not an
authorized query or SDK coverage guarantee. The cursor measures CDC consumption;
during overlap replay, copied rows can already include transactions whose delivery
sequences have not yet been consumed. Ordering is internal serialized-key order,
not PostgreSQL numeric ordering or collation equivalence.

If retention overtakes the replica, `catch_up()` returns `CursorExpired`. Source
mismatch, slot loss, slot acknowledgement ahead of durable state, and incompatible
relation changes also require explicit recovery. Create a fresh source generation,
slot, and replica file for a new bootstrap. Initialization refuses to overwrite an
existing replica. Do not delete/recreate an owned production slot to mask failure.

## Supported scope and limits

- PostgreSQL 18 development privileges and persistent pgoutput v1 streaming.
- Explicit ordinary tables, without inheritance/partitioning, RLS, row filters,
  column lists, generated columns, or schema-wide/all-table publications.
- All four change kinds enabled, default replica identity, a primary key whose
  components are bigint/text/UUID, and boolean/bigint/text/UUID columns. Type
  conversion to logical schema values remains future work.
- At most 64 relations and 128 columns per relation. The whole initial copy and
  live replica have a 100,000-row / 16 MiB encoded row-and-relation payload bound.
  A transaction exceeding the live bound rolls back and keeps its old cursor.
  Row/payload deltas are accumulated and persisted once in the same transaction;
  size checks read those counters instead of scanning all rows. Older replica
  files acquire accounting once on reopen. Composite physical identities are
  encoded as JSON tuples; single-key files retain their existing encoding.
- The snapshot stream bounds accumulated application data, not every driver or
  PostgreSQL allocation. One unusually large source row may be allocated before
  the payload check. A paged/spilled production copy requires further work.
- Publication and schema definitions must remain stable for a generation. Changed
  delivered relation metadata is detected; publication changes that silently stop
  delivering rows are not automatically detected. The source owner must coordinate
  such changes explicitly.
- SQLite WAL/FULL durability is required; volatile databases are rejected. Live
  payload bounds do not promise exact file-size or SQLite WAL caps. Long-lived
  PostgreSQL snapshots retain old versions; cancellation and operational budgets
  need production coverage.

## Verification and measurements

`make cdc-test` includes the snapshot scenarios. They exercise rows predating slot
creation, commits present in both paths, a transaction open at snapshot acquisition,
external writes during the copy, multi-table transactions, key changes, nulls,
unchanged TOAST, deletes, truncate locking, restart, expired handoff, changed schema,
filtered-publication rejection, and failed oversized copies. SQLite fault injection
checks both second-table failure and cursor-commit failure. Pure tests exercise
visibility before/after the WAL fence, failure during initialization, source/cursor
checks, and live replica limits.

`make cdc-bench` includes a 1,000-row copy plus durable initialization and one
100-row update applied to a 1,000-row replica. Connection establishment, snapshot
acquisition, and fixture setup are excluded from the copy case; SQL writes and
CDC capture are excluded from the application case. Each has five warmups and 31
measured samples. See the current baseline and [benchmark methodology](benchmarks.md).
These measure internal durable operations, not authorized subscriptions or UI latency.

`make sync-bench` additionally measures actual command commits at 1/512 tasks,
commit-to-replica lag, the combined latency, bootstrap and internal replica views.
`make cdc-test` covers that same table end to end, including identical logical IDs
in different workspaces, receipt replay, failed version checks, external SQL,
deletes and reopening. There is no comparison engine or shadow table.

## Next boundary

Add exported-snapshot bootstrap to the persistent replication path. Then derive
bounded, authorized query results from the materialized transactions, including
rows entering/leaving results, dependent-policy invalidation, shared execution,
and mutation correlation. Public transport framing, SDKs, configurable mutation
concurrency policies, and schema evolution remain separate work.
