# Managed PostgreSQL functions

`hulladb_postgres::managed` executes normalized user schemas and registered Rust
queries/mutations. It is the generic database execution boundary; it does not
translate operations into the task command enum. The existing root `Store`, task
HTTP host, and task feed remain integration fixtures. The generic
[deployed adapters](sdk-adapters.md) connect this runtime to generated TypeScript
callables through authenticated HTTP/SSE with in-memory mutation confirmation.

## Storage and provisioning

Build a `ManagedSchema` from a generated `SchemaContract` and a dedicated
PostgreSQL namespace. Connect a `ManagedStore`, explicitly call `initialize`, and
provision each data scope with `create_scope`. Connections own their PostgreSQL
client and should be pooled by an embedding host. Managed connections support
verified PostgreSQL TLS, enforce explicit remote transport configuration and bound
connection/statement/lock waits. See [roles, quotas and deployment](production-readiness.md).

The installer creates real PostgreSQL tables with text, boolean, bigint, and UUID
columns. It compiles scoped primary keys, nullability, named logical unique
constraints with explicit null comparison, and scoped foreign keys with restricted
deletes. Tables and columns use ordinal physical names so logical names cannot
inject SQL or collide with scope/version metadata. Logical defaults and UUIDv4/v7
or registered custom key generators execute inside the runtime. Custom generator
outputs pass the complete-row validator before insertion; generator writes share
the enclosing transaction. References are checked immediately: insert referenced
rows first; cyclic declarations are supported, but mutually dependent new rows
cannot be inserted with forward references in one transaction in this slice.

A namespace stores the complete canonical contract and a persisted source UUID.
Initialization, and serving-only `verify`, check an existing contract, including field order, defaults,
generators, references, and uniqueness semantics. It never alters or resets
incompatible storage. This equality check is not schema generation/versioning or
an upgrade migration. Recreate incompatible development fixtures explicitly.
Physical catalog edits and external SQL are outside the managed write boundary.
PostgreSQL text cannot store NUL; managed string writes/predicates reject it.

Operator [visibility and receipt retention](operations-visibility.md) provide
consistent scope diagnostics and bounded result compaction. Mutation receipts store
a SHA-256 fingerprint of the submitted input, rather than retaining that input.
Object-key order does not change this binding; different values, types or array
order do. This fingerprint is duplicate protection, not an input history or an
anonymization mechanism. Browser/native HTTP invocations use short-lived admission
tickets. Result compaction retains duplicate protection until the ticket is retired
and the accepted retention window has passed; it never makes that ticket executable
again. Direct trusted `mutate` calls without tickets keep permanent markers.

## Functions and authorization

`Registry<I>` registers distinct `Query<I>` and `Mutation<I>` handlers and their
mandatory `Authorization<I>` policies. Duplicate function or generator identities
fail registration. Each function implements `FunctionContract` to validate inputs
before execution and results before publication or persistence. Wrong execution
kinds and unregistered operations fail dispatch.

`Context<I>` comes from trusted server integration. It supplies the application
identity, data scope, receipt-owning principal, and authorization scope. The
runtime does not require a membership table or supply login behavior. Scope
selects a physical data partition; application access rules still belong in the
policy/handler. Change the authorization scope when the verified authentication
context changes, so responses from the prior context cannot be recovered as current.
Database permission changes govern new executions and live query refreshes; they
do not retroactively revoke delivery of a committed mutation's original response.

Query handlers and authorization policies receive `QueryContext` with only
`ReadDb::get/select` and the application's identity. Query authorization, data,
source identity, and revision share a read-only repeatable-read transaction.
Mutation handlers receive `MutationContext` with managed reads and
`WriteDb::insert/update/delete`. They cannot access a raw PostgreSQL client or
commit the transaction themselves. The adapter works with canonical `RecordValues`
and `Value`; the [Rust authoring facade](rust-authoring.md) now emits application
row/input/ID structs, checked codecs, typed contexts/selectors and registration
adapters over this same boundary.

Inserts apply literal defaults, validate key policy, generate omitted keys, and
validate the complete row. Updates validate partial inputs and forbid key
assignment; omitted fields remain unchanged and explicit null clears a nullable
field. Updates/deletes require positive expected storage versions. Stale versions
conflict; missing/deleted rows return `NotFound`. Versions are storage metadata,
independent of any authored field with the same logical name.

Selects support schema-validated equality predicates, explicit null equality,
exclusive primary-key keyset bounds, and primary-key ordering. Results are bounded
to 1..1000 rows. All reads track whole-table dependencies, including empty
predicates, point misses, and permission-table reads. A `Snapshot` carries these
conservative dependencies, the result, principal, authorization scope, source, and revision.
Direct query calls always reevaluate. Live subscriptions use `query_round` to
evaluate a captured set at one shared snapshot, with per-member savepoints isolating
ordinary SQL errors. The host reuses the complete round while the scope revision is
unchanged and reevaluates on scope writes or a new subscriber joining. Identity,
source and captured membership fence reuse; credentials are independently rechecked
before delivery. Watched handlers derive results from managed reads and fixed
identity/input; clocks, randomness and external data cannot invalidate these results.
Cached results do not establish local row coverage. See the [HTTP contract](sdk-adapters.md)
for complete-frame publication and failure behavior.

## Atomicity, revisions, and recovery

Default mutation registration uses serializable execution with at most four total
attempts. Same-scope handlers may overlap; eligible concurrency failures replay only
after rollback is confirmed, sharing one execution deadline. Explicit native scope
coordination remains available and acquires the scope revision lock before executing
the handler. Direct native administrative transaction callbacks run once unless
their call explicitly opts into retries. See [concurrency and optimistic
results](concurrency-and-optimism.md) for replay safety, deadlines and remaining
revision contention.

All operations in one handler, custom generator writes, result validation, one
revision advance, and a recovery receipt commit atomically. A mutation that makes
no database writes still receives a receipt but does not advance the revision.
Failure before commit leaves no partial business rows or revision. For admitted
HTTP mutations, a separate transaction records the failure while locking out a
late duplicate. If a duplicate committed first, its success is authoritative.
Transport failure during commit returns
`CommitOutcomeUnknown`. External service side effects must execute outside these
handlers. Use the [transactional outbox](outbox.md) to enqueue them atomically and
deliver after commit through a separate idempotent worker.

Request identities are scoped to data scope and principal. A repeated request
with the same operation/input recovers the stored result without rerunning the
handler. Reusing it for another operation or input fails. Lost acknowledgements
are recovered through the original ticket and request ID on the HTTP service.
The browser SDK automatically looks up the original request; it never resubmits a
mutation. Trusted native storage callers may use `receipt(context, request_id)`. The native duplicate
submission path also returns the saved response after checking the original
operation/input binding.

Confirmation delivers the original authorized response. It checks the currently
verified caller's data scope, principal and authentication scope, together with
the stored source/revision fence and result expiry. It does not rerun handlers,
authorization policies or validators, and does not need the original declaration
to remain registered. A changed declaration still governs new executions. Typed
clients decode the saved response using their own result contract; changing that
contract can still cause a client decoding error. Invalid credentials are rejected
by the host before lookup; another principal cannot retrieve the response.

A scope revision conservatively invalidates every result in that scope. Data and
permission writes both advance it. The [generic function host](sdk-adapters.md)
shares identical watched queries within each host, rechecks authorization on
bounded polling and reevaluates handlers when the source or scope revision changes. It rechecks
credentials before delivery and emits complete replacement snapshots. The
TypeScript adapter retires old results on identity changes, handles reconnects,
keeps results in memory and fences confirmation against source replacement.
Credential checks have an independent cadence and remain specific to each stream.
Tracked dependencies are recorded but do not yet filter unrelated scope writes.

## Validation

```sh
cargo test -p hulladb_postgres --test managed_functions --locked
make db-test
cargo clippy -p hulladb_postgres --all-targets --locked -- -D warnings
```

Database tests use owned namespaces containing publications, articles,
permissions, counters, and UUID markers. They cover multi-operation atomicity,
foreign keys and both null uniqueness policies, defaults and generation,
snapshot consistency during concurrent commits, scope isolation, permission
revocation, stale updates/deletion, duplicate submissions, receipt recovery,
contract mismatch, and fault injection at receipt insertion and commit. Successful
runs drop only their owned namespaces. Use `HULLADB_TEST_URL` to select another
PostgreSQL instance.
