# Bounded CDC snapshot and durable replica

`hulladb_cdc` now copies existing published rows into a separate SQLite replica
and applies retained CDC transactions. PostgreSQL remains authoritative. This is
the internal full-publication materialization, not an authorized client snapshot, a
public SDK, or a general query-serving service.

## Snapshot handoff

`SnapshotReader::begin(url, slot, publication)` creates a **new dedicated durable
slot** and imports its exported snapshot. It rejects an existing slot before
creation; it never advances, resets or drops one to recover a bootstrap.

1. A dedicated SQL connection verifies the source/publication and acquires the
   collector's cooperative advisory lock for the chosen slot name.
2. It discovers the explicit published tables, starts a read-only repeatable-read
   transaction and takes access-share locks **before snapshot export**. `LOCK`
   does not acquire the transaction's MVCC snapshot. Ordinary writes continue;
   TRUNCATE and conflicting DDL wait.
3. A separate bounded replication command connection verifies the same cluster,
   database and publication, then issues `CREATE_REPLICATION_SLOT ... LOGICAL
   pgoutput (SNAPSHOT 'export')`. Creation can wait for already-open writers.
4. The SQL transaction imports the returned snapshot before its first query. The
   exporter stays connected until import succeeds, then closes. The imported
   snapshot remains valid while the SQL copy transaction is open.
5. `reader.source()` returns the new generation, source identity and consistent
   starting LSN. Persist this identity with the log and replica for recovery.
   `read()` streams supported columns with the same native text settings as CDC;
   booleans are `t`/`f`, and null stays distinct from text.
6. `Replica::initialize()` installs all relations, rows, source identity and cursor
   zero in one SQLite WAL/FULL transaction. Failed or oversized copies never return
   a complete `Snapshot`; failed initialization never publishes a partial ready state.
7. Capture starts from the slot's consistent point. PostgreSQL aligns that stream
   with the exported snapshot: copied commits are excluded, and subsequent committed
   transactions are applied exactly once. No saved XID visibility list or overlapping
   WAL fence is needed. Materialization is independent of PostgreSQL's XID epoch,
   including 32-bit pgoutput XID wrap.

The command connection reuses pgwire's SCRAM/authentication primitives, supports
the same single-host non-TLS development transport, and never receives or
acknowledges WAL. It rejects backend frames above 64 KiB before payload allocation,
requires one bounded result row, and uses five-second connection/command deadlines.
Streaming still uses the existing persistent capture transport.

See PostgreSQL's [replication slot/export protocol](https://www.postgresql.org/docs/18/protocol-replication.html),
[snapshot import rules](https://www.postgresql.org/docs/18/sql-set-transaction.html),
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
authorized query or SDK coverage guarantee. Cursor zero represents the copied state at the exported slot boundary; later
sequences measure whole transactions applied from its tail. Ordering is internal serialized-key order,
not PostgreSQL numeric ordering or collation equivalence.

If retention overtakes the replica, `catch_up()` returns `CursorExpired`. Source
mismatch, slot loss, slot acknowledgement ahead of durable state, and incompatible
relation changes also require explicit recovery. Create a fresh source generation,
slot, and replica file for a new bootstrap. Initialization refuses to overwrite an
existing replica. Replica format two records the exported boundary; older
format-one overlap replicas fail explicitly and require a fresh bootstrap, since
removing their replay filter would change recovery semantics. Their files are not
rewritten or deleted.

Dropping or cancelling a bootstrap closes its owned connections and releases table
locks. PostgreSQL may still finish slot creation after a client disconnect, so an
interrupted command can leave an inactive durable slot. Inspect only the owned slot,
wait for the exporting backend to exit, and select a fresh name/generation for a
retry. A failed copy leaves its slot at the consistent point and does not acknowledge
any tail. Never delete/recreate an existing production slot to mask failure.

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
  size checks read those counters instead of scanning all rows. Current-format replica
  files missing accounting acquire it once on reopen. Composite physical identities are
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
  PostgreSQL snapshots retain old versions. Export cancellation and lock release
  are tested; total copy deadlines, spill and operational budgets still need
  production coverage.

## Verification and measurements

`make cdc-test` includes rows predating slot creation, a writer open before export,
external commits after import/during copy, multi-table transactions, key changes,
nulls, unchanged TOAST, deletes, TRUNCATE locking before and after export, restart,
expired handoff, changed schema, existing-slot rejection, failed oversized copies,
and cancellation while slot creation waits. SQLite fault injection checks both
second-table failure and cursor-commit failure. Pure tests cover materialization
across pgoutput XID wrap/reopen, legacy-format rejection without rewriting rows,
initialization failures, malformed/oversized bootstrap frames, authentication
ordering, source/cursor checks and live replica limits. A real cluster beyond the
first XID epoch has not been manufactured for the local integration tests.

`make cdc-bench` includes a 1,000-row copy plus durable initialization and one
100-row update applied to a 1,000-row replica. Connection establishment, slot creation, snapshot
import, and fixture setup are excluded from the copy case; SQL writes and
CDC capture are excluded from the application case. Each has five warmups and 31
measured samples. See the current baseline and [benchmark methodology](benchmarks.md).
These measure internal durable operations, not authorized subscriptions or UI latency.

`make sync-bench` additionally measures actual command commits at 1/512 tasks,
commit-to-replica lag, the combined latency, bootstrap and internal replica views.
`make cdc-test` covers that same table end to end, including identical logical IDs
in different workspaces, receipt replay, failed version checks, external SQL,
deletes and reopening. There is no comparison engine or shadow table.

## Next boundary

Derive bounded, authorized query results from the materialized transactions, including
rows entering/leaving results, dependent-policy invalidation, shared execution,
and mutation correlation. Public transport framing, SDKs, configurable mutation
concurrency policies, and schema evolution remain separate work.
