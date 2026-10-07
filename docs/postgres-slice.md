# Authoritative PostgreSQL commands and queries

`hulladb_postgres` owns the fixed task backend: authorization, indexed bounded
reads, version-checked commands and durable request receipts. PostgreSQL is
source of truth. This backend exposes durable managed heads and consistent
query snapshots to `hulladb_sync`; it has no raw payload feed or server row replica.
These crates are internal and unpublished. The [task host](task-host.md) supplies
authenticated HTTP/SSE, bounded pooling and scheduled recovery. General schema
adapters, native PostgreSQL TLS and SDKs remain unimplemented.

```sh
make db-test
make db-bench
make db-bench-compare
make sync-test
make sync-bench
```

Use `HULLADB_TEST_URL` to override the local Compose database. The fixed namespace
is `_hulladb_task_probe`; fixtures own UUID workspaces and remove only their rows.
The namespace name identifies existing development storage, not a second engine.

Commands validate input and authorize a trusted principal/workspace session.
Create rejects duplicate scoped identities. Patch/delete require the expected
version; stale edits conflict and deleted tasks cannot be resurrected by an
update. Task rows, query revision and receipt commit atomically. A receipt records
one command result, the original deployment source UUID and a query freshness
revision. Its portable visibility fence can be compared with complete client-query
progress; recovery never rebinds the receipt to a new source identity. Membership
changes advance that revision atomically too. A receipt is never a sync event.
The backend returns after PostgreSQL commit, without awaiting client
visibility. An unknown commit outcome is recovered with the same request ID.

Request IDs are scoped to workspace and principal. Identical retries recover the
stored receipt; changed arguments fail. Reads and recovery recheck current
membership. Managed writes still lock one workspace to order receipt checks and
query revisions; this is a conservative correctness baseline. Narrower locking
is separate work and must preserve operation invariants and atomic invalidation. The command path no longer
inserts a redundant delivery batch.

`list_tasks` uses bounded indexed UUID keyset pages. `query_snapshot` returns only
`id/title/completed`, complete for the exact bounded query and authenticated scope.
Authorization, rows and the managed-query revision share a repeatable-read
snapshot. Query revisions do not track arbitrary external SQL. See
[coverage semantics](query-coverage.md); the authorized feed polls durable revisions
and reevaluates PostgreSQL queries. Delivery cursors remain independent of these managed revisions.

The initializer creates the current schema and a stable deployment UUID, serializing
installation with an advisory transaction lock. There is no pre-publication schema
versioning or migration path: recreate incompatible development fixtures explicitly.
A restore or out-of-band change requires source rotation and fresh subscriptions.

The actual task table retains its physical `(workspace,id)` key. Authorized
queries filter that scope before returning rows for client SQLite. The optional
diagnostic CDC bootstrap/replica supports the composite key too, but does not
participate in serving. The logical SDK schema still selects one primary-key
field. Physical scoping is internal storage metadata.

Database scenarios cover lost-response recovery, duplicate concurrent requests,
version races, receipt-failure rollback, deletion, invalid inputs, authorization,
keyset bounds and top-N refresh/revocation. The managed
integration covers revision polling, permission invalidation, bootstrap races and
client restart without replication. External SQL requires maintenance/reset and
is not a supported live mutation path. See [correctness and recovery](managed-sync.md).
Current latency cases and baselines are described in [benchmarks](benchmarks.md).
