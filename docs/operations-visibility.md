# Operational visibility and receipt retention

The registered-function runtime exposes process-local host statistics, consistent
operator snapshots of each scope, and bounded cleanup of remote mutation outcomes.
There is no public diagnostics route. Application operators choose how to collect
statistics and alerts; the host automatically maintains ticketed request retention.

## Host statistics

`ManagedHost::stats()` returns a serializable `HostStats` without SQL or network
work. Read it through an application-owned operator interface or export it to
monitoring. Counters have no bearer, scope, principal, function-name, argument,
result or job-payload labels. The self-hosted starter writes JSON records with
`event: "hulladb.host_stats"` to stdout every 30 seconds;
`HULLADB_STATS_INTERVAL_SECONDS` accepts 1..86400 seconds. Reporting ends with the
server lifecycle and does not install a separate worker.

The snapshot includes:

| Fields | Meaning |
| --- | --- |
| `requests_started`, `requests_completed`, `requests_cancelled` | Function-router requests, including authentication/admission failures. Cancellation means the request future ended before response headers. |
| `request_duration_us` | Cumulative time through response headers, including cancelled requests; excludes SSE body lifetime. Use deltas for an average, not a latency percentile. |
| `successful_responses`, `client_error_responses`, `server_error_responses` | HTTP status classes; successful SSE admission counts once. |
| `capacity_responses`, `operation_deadlines`, `unknown_outcomes`, `expired_receipts` | HTTP capacity responses, host request/stream refresh deadlines, HTTP unknown-outcome responses and HTTP expired-result responses. Readiness probes are excluded. |
| `admission_cleanup_failures` | Failed or timed-out cleanup batches; the next scheduled batch retries. |
| `active_requests`, `active_subscriptions`, `subscription_groups` | Admitted request permits, live stream-body permits and shared query groups. A terminal stream retains its permit until its body is dropped. |
| `database_in_use`, `database_waiters`, `database_reconnects` | Pool leases, callers awaiting a slot and reconnect attempts after an unusable connection. Cancellation releases waiter accounting. |
| `subscription_refreshes`, `coalesced_refreshes`, `unchanged_refreshes` | Database refresh attempts, reuse of a group snapshot before its next poll, and refreshes returning the same snapshot. Refreshes include authorization/cache checks, not necessarily handler execution. |
| `snapshots_delivered`, `snapshot_bytes`, `stream_failures` | Snapshot events emitted to polled bodies, their encoded JSON bytes, and terminal SSE error events. These do not prove client persistence or receipt visibility. |

Counters reset on process restart and saturate at `u64::MAX`. Individual reads
are safe under concurrency, but the combined host snapshot is approximate.
Health routes outside the function router are excluded. Network disconnects
without an SSE error are not counted as terminal stream failures.

## Scope diagnostics

An operator connection calls `ManagedStore::scope_status(scope)`. The method
requires schema CREATE privilege, checks that the scope exists and reads source,
revision, configured quota, accounted usage, receipt state and outbox state in
one repeatable-read snapshot. It returns counts and ages rather than payloads.
The runtime, worker and backup groups cannot use this operator API.

Receipt state separates retained results from expired request markers and reports
`oldest_retained_age_ms`, or `None` when no retained results exist. Age starts at
the receipt's database insertion timestamp. Usage includes both retained receipts
and markers, admission records with a terminal-failure reserve, tickets, and all
retained outbox jobs.

Outbox states are disjoint:

- `ready`: current-source, available jobs with attempts remaining and no live lease.
- `delayed`: the same eligibility, with a future availability time.
- `leased`: current-source, nonfailed jobs with a live lease.
- `dead`: current-source failed jobs or exhausted jobs without a live lease,
  including final-attempt crashes awaiting the next claim's maintenance pass.
- `old_source`: every job fenced to a previous source, excluded from the other states.

`oldest_ready_age_ms` measures time since the oldest eligible job's availability,
including its latest retry delay; it is absent when no jobs are ready. Source
rotation makes retained work visible as old-source backlog without making it
eligible for delivery. Reconcile it through the explicit [outbox APIs](outbox.md).

Scope accounting scans data in that scope. Collect it at an operator-selected
cadence and measure its cost at representative sizes. Byte budgets exclude WAL,
indexes and filesystem overhead; they are not disk-usage measurements.

## Automatic remote outcome retention

The SDK obtains a reusable opaque ticket during session setup. The current service
issues five-minute tickets; the browser renews after four minutes for future calls.
Each invocation retains its original ticket. A mutation does not need a separate
application preflight, and ticket expiry never authorizes resubmission.

First admission binds the function, input fingerprint and verified identity. The
request has 15 seconds to start execution. A running transaction holds its request
lock, so cleanup cannot fence or purge it while it is active. Success commits with
the business writes; failure is recorded only after rollback and acquiring that
same lock. Abandoned pending requests become a durable execution-limit failure once
cleanup acquires the lock after their start deadline.

The accepted record fixes a 60-second result-retention window after completion and
30 seconds of cleanup grace. Compact request markers remain until both their
accepted retention deadline and permanent retirement of the issuing ticket. The
host starts cleanup once a second, selecting at most 100 rows per stage and
skipping active locks. Full batches continue within one bounded maintenance turn,
releasing the pool connection between batches. A stopped or overloaded host can delay physical reclamation;
lookup still enforces result expiry. PostgreSQL vacuum, indexes, WAL and backups
have their own storage lifecycles.

After safely receiving a success, clients send a best-effort acknowledgement that
releases the result payload early. A lost acknowledgement is harmless: automatic
expiry releases it later. ACK never releases the request's duplicate-execution
fence. Retained failure codes contain no handler inputs or internal database errors.

HTTP **410 `receipt_expired`** means a success was committed but its result is no
longer retained. HTTP **410 `ticket_expired`** rejects new admission using an expired
or retired ticket; it does not assert that a mutation committed. Missing outcomes
remain nonterminal. The browser continues lookup within its default 30-second total
confirmation budget, then reports a confirmation error without claiming rollback.
Neither client resubmits a pending invocation to obtain a result.

These timings are current service implementation defaults, not an audit-history
policy or a new application configuration surface. Keep business history in
application tables if it is needed beyond this short confirmation lifecycle.

## Explicit compaction for trusted native calls

The lower-level trusted `ManagedStore::mutate` API without tickets still retains
permanent duplicate markers, since it has no expiring admission identity. Its
operator `compact_receipts(scope, source, minimum_age, limit)` can discard results
but cannot safely delete those markers. The HTTP service always requires tickets.

Compaction replaces result JSON with `null` and preserves the request binding.
Choose the minimum age for the application's required recovery window; the batch
limit is 1..1000. It checks the current source, leaves business rows/revisions
unchanged and returns `ReceiptExpired` for compacted successful invocations.

Backup/restore preserves accepted timestamps and rotates the source without
rebinding old requests. Incompatible development layouts are rejected explicitly;
no existing namespace is migrated or reset automatically.

## Self-hosted operator commands

The starter uses `HULLADB_ADMIN_DATABASE_URL_FILE` and `HULLADB_NAMESPACE` for
these commands. It verifies existing storage and never initializes it implicitly.
Using the self-hosted Compose file, run:

```sh
docker compose -f deploy/selfhost/compose.yaml run --rm initialize status "$SCOPE_UUID"
docker compose -f deploy/selfhost/compose.yaml run --rm initialize compact-receipts "$SCOPE_UUID" "$EXPECTED_SOURCE_UUID" 2592000 100
```

Copy the expected source from the status output. Compaction prints the number of
results compacted in that batch. Neither command starts serving or a delivery
worker. Run them with the operator credentials; the serving process continues to
use its separate runtime credentials.

Useful alerts include sustained pool waiters, capacity responses, operation
deadlines/unknown outcomes, ready-job age, dead/old-source backlog and quota
consumption. Set thresholds from actual workload and recovery requirements.
No monitoring backend, alert destination or maintenance schedule is configured
by the library.

## Validation

`make operations-visibility-test` runs PostgreSQL retention/privilege/queue-state
cases, host HTTP/SSE diagnostics and both client recovery paths. Existing
`make production-test` includes the real backup/restore drill, which preserves
expired markers as well as retained receipts.
