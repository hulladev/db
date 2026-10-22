# Transaction concurrency and optimistic results

Managed Rust mutations default to serializable execution and bounded retries.
Generated TypeScript callables can opt into ordered predictions over exact cached
query results. Both paths preserve the original request identity and authoritative
PostgreSQL transaction/receipt boundary.

## Transaction execution

`Registry::mutation` defaults to serializable execution with up to four total
attempts: the initial attempt and three retries. TypeScript worker registration
uses this default too. Explicit native scope coordination remains available through
`mutation_with_concurrency`; it locks the scope revision before the handler.

Ordinary native `AdminDatabase::transaction` runs once. Opt into additional attempts
with `transaction_with_options(..., TransactionOptions { retries, ..Default::default() })`.
This is the existing native administrative surface, not a claim that the planned
TypeScript direct database builder has landed. Options are per call.

`Registry::mutation_with_concurrency` accepts
`MutationConcurrency::Serializable(RetryOptions)`. Generated Rust mutations expose
the same choice through their `register_with_concurrency` method:

```rust,ignore
use std::time::Duration;
use hulladb::{MutationConcurrency, RetryOptions};

create::register_with_concurrency(
    &mut registry,
    application_policy,
    MutationConcurrency::Serializable(RetryOptions {
        max_attempts: 3,
        timeout: Duration::from_secs(2),
        backoff: Duration::from_millis(10),
    }),
)?;
```

This mode runs the complete authorization/handler/generator transaction at
PostgreSQL serializable isolation. Handlers in one scope can overlap without
acquiring the scope row lock up front. The final revision increment and receipt
still commit with the data. Concurrent changes that invalidate a snapshot cause
PostgreSQL to reject the attempt; retries recheck current authorization and data.
The scope revision remains a final commit contention point, so independent
same-scope commits can still cause serialization retries. This is not a claim of
uncontended row-level commits or a measured throughput improvement. Resource-level
coordination and finer revision partitioning remain separate work.

Registered mutation handlers, policies and custom generators must be safe to replay.
Direct server callbacks require explicit retry opt-in. Keep external effects outside
managed handlers. Only SQLSTATE `40001` (serialization failure) and `40P01`
(deadlock) trigger internal retries, after a confirmed rollback. Validation,
expected-version/uniqueness conflicts, permission denial and unknown commit
outcomes are never automatically replayed. A duplicate submission that loses a
unique or serialization race checks the original operation/input receipt in a
fresh authorized snapshot before deciding whether to retry.

Budgets allow 1..16 attempts and a total deadline up to 60 seconds, including lock
waits, execution and exponential jittered backoff. `RetryExhausted` means no
attempt committed. `TransactionDeadline` means cancellation occurred before
COMMIT. A deadline after COMMIT starts returns `CommitOutcomeUnknown` and requires
receipt recovery. Every attempt uses the original principal, source expectation,
request ID and arguments; no retry substitutes a newer expected row version.
Generated IDs from rolled-back attempts can change because they were never
published. Host/client admission and request deadlines apply independently.

## TypeScript optimistic updates

Generated mutations expose `.withOptimistic(...targets)`. Each descriptor supplies
`query` and a pure `update(current, mutationInput, queryInput)` returning a
replacement result. No-input queries need no input mapping. For a query with input,
provide an `input(mutationInput)` mapper for one exact input, or a
`match(queryInput, mutationInput)` predicate selecting already-cached inputs.
Matching does not require an input mapper. The wrapper preserves the mutation's
normal input and authoritative output types. Use the original callable for the
non-optimistic path; mutation calls have no mode override.

Missing results are skipped and cannot acquire the prediction later. Null and empty
arrays are real cached results, not absence. Updaters execute synchronously and may
rerun; capture temporary IDs or other nondeterministic values before the updater.
Their results are validated against generated query contracts. An invalid update or
exception removes all targets for that invocation and leaves server execution alone.
Development diagnostics omit query and mutation data.

Layers apply in submission order over the latest confirmed base. Removing one layer
replays surviving layers rather than restoring an obsolete snapshot. Remote writes
are preserved. There is no explicit dependency graph or command rewriting. Updaters
return immutable replacements; query results are readonly.

## Visibility and confirmation

A successful receipt establishes a source/revision fence. Its layer remains for
each exact query until an authoritative snapshot incorporates the commit. This also
handles filtered queries that exclude the committed row. Server canonical values
replace provisional ones without applying the same prediction again.

When SSE arrives before acknowledgement, the HTTP adapter checks unresolved receipt
fences before installing that snapshot. A newer snapshot arriving during this work
cannot be overwritten by the older one. This existing per-query correlation does
not establish coordinated publication across multiple queries.

Transport interruption immediately removes the prediction while the original
mutation handle enters recovering. Automatic receipt checks continue within its
confirmation deadline and never resubmit the payload. Successful recovery does not
reapply an abandoned layer. Confirmed execution errors settle error; a confirmation
timeout is not proof of rollback. Live queries independently refresh server state.
No browser journal or predictions survive reload.

## Validation

Use focused managed_functions tests for default OCC, confirmed rollback before replay,
contention/exhaustion and commit uncertainty; admin tests cover run-once versus
explicit retry options. PostgreSQL cases are ignored unless explicitly enabled and
require a database. TypeScript tests cover layered rejection, missing-cache behavior,
canonical visibility, updater failure, no-resubmit reconciliation, deadline handling
and identity changes. Browser and deployed fixtures use the same generated wrappers.
Performance gains have not been measured.
