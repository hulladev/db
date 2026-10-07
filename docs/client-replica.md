# Native authorized client SQLite persistence

`hulladb_client::TaskReplica` is the native persistence core for the fixed task serving
slice. It depends on portable query/delivery contracts and SQLite, never PostgreSQL,
replication credentials, LSNs or raw relation metadata. PostgreSQL remains authoritative.
This is an internal native client core, not a complete SDK or a browser/Wasm adapter.

## Data and coverage

One SQLite database binds to one principal/workspace. Separate exact query registrations
retain authorized `id/title/completed` rows, a lifecycle cursor and source/revision visibility progress. No physical PostgreSQL
row, task command version or other workspace enters the client store. Overlapping queries
currently retain independent rows; shared membership/projection accounting is future work.
Unsubscribing or resetting one query cannot delete another query's retained rows.

Register the subscription ID obtained from the authenticated subscription handshake. Read only
exact query arguments. A registered but unfinished snapshot has no coverage; a completed
empty page is covered. Reopening restores historical coverage, never a connectivity or
freshness guarantee. Disconnection/loading state belongs to the transport/SDK layer.

## Streamed bootstrap and updates

`begin_snapshot(query, cursor, visibility)` starts a SQLite transaction for one
registered delivery subscription ID, using the source/revision paired with the rows.
`SnapshotWriter::append` accepts bounded chunks and validates projection, ordering,
keyset/filter membership, and total query cardinality. Rows are written directly to SQLite;
there is no Vec containing the whole database. The current internal serving adapter sends
one complete bounded page. The [authenticated task host](task-host.md) sends that
page in a complete SSE message; network row-chunk framing is not implemented yet.
A future receiving adapter can append decoded chunks without retaining a whole bootstrap.

`finish` is the end-of-snapshot publication point: rows, recent delivery chunks, cursor and visibility progress
commit together in WAL/FULL mode. Other readers see the previous complete page until then.
Dropping/cancelling the writer, a malformed chunk, budget failure or cursor-write failure
rolls back the entire replacement. A failed writer cannot publish a valid prefix.
Full coalesced replacements can skip delivery sequence numbers. Exact replay is idempotent;
conflicting replay (including conflicting progress), older sequences, foreign sources,
regressing revisions and unregistered subscription IDs fail.

Delivery cursors order query lifecycles; they are not CDC positions or receipt revisions.
A new subscription ID must be registered explicitly before replacing a query after reset.
Revocation clears all rows and retained event payloads for this scope, closes the database
lifecycle durably, and rejects late results. A later authenticated regrant needs a fresh
client database lifecycle; it cannot reopen a revoked one. This describes logical cache
removal, not secure erasure of filesystem pages or data already copied by the application.

## Committed versus visible

Pass `Receipt::visibility_fence()` from the trusted command response to
`is_visible(session, query, fence)`. Only complete persisted coverage for that exact
query can satisfy it. A complete snapshot evaluated at a later revision includes the
commit in query evaluation, even if filters or top-N membership omit the changed row.
The check reads only metadata and never applies receipt payloads as synchronized rows.

Partial, cancelled or failed snapshots cannot advance progress. Coalescing can
satisfy multiple receipts at once. Reopening preserves historical visibility, without
claiming current server freshness. Registering a new lifecycle first drops old coverage.
A fence from a replaced source cannot be satisfied by that source's replacement.
Revocation/reset prevents further visibility claims through the closed lifecycle.
Incompatible existing development SQLite files require explicit recreation, not migration.

## Resource policy

`ReplicaOptions` requires explicit chunk row/encoded-byte budgets, recent deliveries
retained per query, and SQLite busy timeout. There is no machine-independent default
100,000-row/16 MiB total storage ceiling. History retention is separate from current rows;
zero disables history without discarding current coverage. Encoded chunk bytes do not
measure total process memory, SQLite cache, filesystem size or WAL growth. Single-row and
transport preallocation limits, disk quotas and dedicated worker scheduling remain separate
resource work. Query cardinality still follows the existing fixed 1–100 contract.

Tests cover a stored working set above 100,000 rows and 16 MiB using small chunks,
concurrent reads during publication, cancellation, SQLite failure rollback, reopen,
late/conflicting deliveries, revocation, byte budgets and bounded history. Run:

```sh
cargo test -p hulladb_client --locked
```
