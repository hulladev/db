# Explicit projections and bounded query coverage

This chunk adds entity projection contracts to `hulladb_schema` and one bounded
task-summary query/cache to the authoritative PostgreSQL backend. APIs remain internal and
provisional. There is no service or pushed subscription transport yet.

## Projection contracts

`TableContract::projection(name, fields)` resolves an explicit nonempty field
selection against a normalized table. An entity projection must include the
selected primary key. Unknown names, repeated fields, and malformed selections
are errors. The field order is preserved, and newly added table fields do not
become part of an existing projection.

`ProjectionContract::validate_record()` requires every selected field, enforces
its scalar type and nullability, and rejects unselected fields. An omitted
nullable field is incomplete; explicit null is a value. Insert defaults are
removed from the projection and never fill missing result fields. Full-row,
insert, patch, and projected-record validation remain separate operations.

These are entity projections. Aggregate rows, computed fields, joined/nested
results, generated frontend types, and stable schema identities remain future
contracts.

## Fixed task-summary query

`Store::query_snapshot(session, TaskQuery)` executes the fixed named task-summary
query. Arguments are an optional completion filter, optional exclusive UUID
keyset bound, and a limit of 1–100. Ordering is ascending task ID. The projection
is always `id`, `title`, and `completed`; the distinct `TaskSummary` type has no
version field and rejects extra fields when deserialized. SQL fetches only the
declared columns. Version-guarded commands can obtain versions through the
existing full-task reads.

Authorization, workspace sequence, and rows are read in one repeatable-read
transaction. The result is complete for the exact bounded query at that revision,
even if the workspace has far more rows. Fifty returned tasks establish coverage
for that top-50 query, not for all tasks or a different page. The retired task-feed replica is no longer part of this backend.

The `QueryRevision` covers managed command freshness only; it is not a CDC
resume cursor or proof of external-write visibility. Raw CDC materializations
remain internal. Authorized CDC query serving must unify coverage, policy
dependencies and external-write invalidation before SDK delivery.

## Cache identity and state

Each `QueryCache` owns one query and one trusted principal/workspace session.
Projection identity is fixed by the task-summary result type; schema generation
is checked in its revision. Reads require the matching session and exact query
arguments. Different principals/workspaces fail authorization, and different
filters, keyset bounds, or limits fail with `QueryNotCovered`. No query containment
or arbitrary local evaluation is inferred from a page.

`QueryView` exposes borrowed results, optional `complete_at`, the latest observed
revision, and a status. These separate data availability and historical completeness
from loading, known staleness, disconnection, or failure. A loading empty cache
has no completeness revision. An authoritative empty result has one. `Ready` means
the installed snapshot reaches the latest observed revision; it does not prove
that there have been no unobserved server changes.

Installing a snapshot validates scope, query identity, generation, row count,
projection values, predicate, keyset bound, and strict ordering before changing
visible state. Older responses cannot regress a newer result. A response behind
an already observed revision remains stale. Contradictory results at the same
revision are rejected. Incoming snapshots are trusted server responses; these
structural checks do not authenticate or cryptographically prove completeness.

## Refresh and revocation

The caller reports a workspace commit through `observe(revision)`, then requests a
new bounded snapshot. Every newer workspace commit conservatively invalidates
this cache, including unrelated changes. This is an explicit correctness-first
baseline; dependency-based invalidation and sharing execution across subscribers
are the next scaling work. There is no automatic polling/refresh scheduler here.

Refresh re-executes the indexed bounded server query. When a top-N task is deleted
or leaves its filter, the server can return a replacement outside the previously
delivered result. Applying only deltas for delivered rows would miss that task.
Historical rows can remain visible during refresh, with their previous revision
and an explicit status.

Disconnection and ordinary failures preserve historical results. On an
authoritative permission failure, `fail(Error::Forbidden)` erases retained data
and closes that cache. Late responses cannot repopulate it. A newly authenticated
lifecycle creates a new cache. Callers must deliver permission failures to the
cache; membership changes are not yet pushed by a serving layer. Auth
policy dependencies, account lifecycle integration, and immediate revocation
notification remain required before a service/SDK can promise that behavior.

## Verification and next boundary

Projection scenarios cover missing/extra fields, nullability, types, defaults,
custom primary keys, and malformed selections. Cache scenarios cover exact
coverage, loading versus complete-empty, stale/delayed responses, scope isolation,
malformed snapshots, errors, disconnection, and revocation. PostgreSQL tests check
a two-row summary in a 1,000-task workspace, correct top-N replacement, revoked
reads, and atomic receipt failure rollback.

The PostgreSQL harness adds 50-row summary snapshots at 1,000 and 100,000 rows
and a batched cached-result access case. Cached-result access does not allocate
or evaluate a new query, and is not a browser interaction benchmark. Current backend timings are recorded in `benchmarks/baselines/backend-local.json`.

Next, define versioned service messages and bounded delivery/backpressure, then
test dependency invalidation, shared execution, and commit-to-other-client
latency under fan-out. Browser local queries and optimistic feedback need their
own workloads when their implementations exist.
