# Authoritative PostgreSQL commands and queries

`hulladb_postgres` owns the fixed task backend: authorization, indexed bounded
reads, version-checked commands and durable request receipts. PostgreSQL is
source of truth. Synchronization belongs exclusively to `hulladb_cdc`; this
backend has no change-feed API, delivery-log table or in-memory sync replica.
These crates are internal and unpublished. General schema adapters, service
transport, production TLS/pooling and SDKs remain unimplemented.

```sh
make db-test
make db-bench
make db-bench-compare
make cdc-test
make sync-bench
```

Use `HULLADB_TEST_URL` to override the local Compose database. The fixed namespace
is `_hulladb_task_probe`; fixtures own UUID workspaces and remove only their rows.
The namespace name identifies existing development storage, not a second engine.

Commands validate input and authorize a trusted principal/workspace session.
Create rejects duplicate scoped identities. Patch/delete require the expected
version; stale edits conflict and deleted tasks cannot be resurrected by an
update. Task rows, query revision and receipt commit atomically. A receipt records
one command result plus a query freshness revision. It is never a sync event.
The backend returns after PostgreSQL commit, without awaiting capture or replica
visibility. An unknown commit outcome is recovered with the same request ID.

Request IDs are scoped to workspace and principal. Identical retries recover the
stored receipt; changed arguments fail. Reads and recovery recheck current
membership. Managed writes still lock one workspace to order receipt checks and
query revisions; this is a remaining contention cost to optimize when CDC query
serving supplies coverage and visibility correlation. The command path no longer
inserts a redundant delivery batch.

`list_tasks` uses bounded indexed UUID keyset pages. `query_snapshot` returns only
`id/title/completed`, complete for the exact bounded query and authenticated scope.
Authorization, rows and the managed-query revision share a repeatable-read
snapshot. Query revisions do not track arbitrary external SQL. See
[coverage semantics](query-coverage.md); authorized CDC serving must handle those
changes and policy dependencies before clients use it.

Internal schema generation two atomically migrates existing generation-one
receipts into the command-result format, preserves rows/versions and request
arguments, and drops the obsolete `changes` table. Unknown generations and
malformed prior receipts fail. The initializer serializes schema installation
with an advisory transaction lock; it is not a general migration compiler.

The actual task table retains its physical `(workspace,id)` key. CDC bootstrap
and replica materialization now support that composite identity directly, with
no shadow table or mirror trigger. The logical SDK schema still selects one
primary-key field. Physical scoping is internal storage metadata.

Database scenarios cover lost-response recovery, duplicate concurrent requests,
version races, receipt-failure rollback, deletion, invalid inputs, authorization,
keyset bounds, top-N refresh/revocation and schema upgrade preservation. The CDC
integration covers commands and external SQL through the same durable path.
Current latency cases and baselines are described in [benchmarks](benchmarks.md).
