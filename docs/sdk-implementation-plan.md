# SDK replacement delivery plan

Prepared 16 October 2026 from the agreed architecture and three read-only agent
reviews of the implementation, correctness guarantees and website documentation.
The initial inventory below predates implementation; current progress and proof
results are recorded at the end of this document.

The governing contract is [sdk-architecture.md](sdk-architecture.md). Detailed
accepted edge cases remain in [backend-functions.md](backend-functions.md), whose
historical implementation sections are not instructions to preserve old APIs.

## Outcome and completion criteria

Deliver one coherent `@hulla/db` implementation with two entry paths:

1. An application server imports the generated database, performs CRUD and groups
   writes in `db.transaction`; its framework owns auth and external services.
2. A browser imports generated query/mutation callables, observes live results and
   optionally wraps mutations with `.withOptimistic`.

Both paths use the same storage constraints, transaction machinery and change
invalidation. Package internals own discovery, registration, wire handling and
confirmation. The first application never copies runtime plumbing from a test.

Completion means both paths run against a user-defined schema from documented
setup through persisted results, browser subscriptions observe direct server
writes, auth changes isolate results, confirmed mutations and rollback behave as
documented, and all accepted v1 schema/query features have implementation coverage.
The first vertical slice is a milestone, not permission to omit the remaining
accepted surface. Retired implementation paths, exports and examples are removed.

Portable cloud support is a v1 release gate: demonstrate generated direct `db`
operations and transactions from a deployed application host, alongside browser
live queries through a persistent service and shared PostgreSQL. Next/Vercel is one
representative fixture; reasonable Node/Bun cloud environments share the same design.
Local Node compatibility is necessary but does not satisfy this gate. See the
deployment proof packet below for remaining work.

Do not maintain two runtime versions, compatibility aliases, an old SDK behind a
flag, schema generations or pre-publication upgrade migrations. Git history is the
archive. Explicitly recreate incompatible development fixtures; never reset data
silently. Preserve source identities, revisions and concurrency versions.

## Verified baseline

The checkout is already substantially modified and contains untracked runtime and
documentation files. Inventory those changes before each implementation packet;
do not reset, stash away or overwrite unrelated work. This plan does not establish
a clean Git baseline or claim the existing tests pass.

| Area | Reuse | Replace or extend |
| --- | --- | --- |
| Generic PostgreSQL engine | `managed/store.rs`, `managed/db.rs`: snapshot/transaction ownership, constraints, revisions, table dependency collection | Statement/query AST, accepted data types, failed-query dependencies, direct server scopes, default OCC policy |
| Canonical schema and wire | `hulladb_schema`, `hulladb_wire`, TS schema/compiler ownership checks | Current scalar support is narrow; implement JSON, dates, numbers, constraints, codecs and inferred adapter schemas |
| Function execution | Native TypeScript transaction bridge, bounded channels, closed-scope protections, Standard Schema declarations/extensions, output-before-commit | Config/limit wiring, actual restriction enforcement |
| Code generation | Owned-output checks, deterministic generation, browser type-only handler imports | Discovery/configuration and agreed generated entrypoints; remove manual declarer setup |
| Managed service | HTTP/SSE, credential boundary, pools, quotas, exact-query subscription sharing | Mounted verifier with nullable identity, coordinated rounds, new receipt protocol |
| Client runtime | Useful codec/transport internals and optimistic base/layer ideas | Public runtime setup, native-Promise/.watch surface, persisted intents, replay recovery, modes and error classes |
| Tests | Snapshot, rollback, contention, auth race and fault-injection scenarios | Assertions enforcing retired semantics; generated fixtures and native consumers move with their contracts |

Initial source evidence (superseded items are listed in the current status below):

- [server.ts](../packages/db/src/server.ts): the initial foundation exposed manual
  factories and positional validators; generated constructors and Standard Schema
  declarations have since replaced application authoring with that syntax. Separate native authorization is
  registered in [managed/functions.rs](../crates/hulladb_postgres/src/managed/functions.rs)
  and [managed/typescript/mod.rs](../crates/hulladb_postgres/src/managed/typescript/mod.rs).
- [database.ts](../packages/db/src/database.ts): eager CRUD and `Stored` version
  wrappers are not the agreed awaitable builder API.
- [http.ts](../packages/db/src/http.ts): the original IndexedDB, manual recovery and
  receipt-missing resubmission paths have been removed.
- [callable.ts](../packages/db/src/callable.ts) and
  [contracts.ts](../packages/db/src/contracts.ts): native Promises and `.watch` have
  been replaced by immediately-started observable awaitable handles.
- [concurrency.rs](../crates/hulladb_postgres/src/managed/concurrency.rs): declared OCC defaults and
  confirmed-rollback retry handling have since replaced the original scope-serialized
  default. Explicit native direct transactions still default to one attempt.
- [subscriptions.rs](../crates/hulladb_server/src/managed/subscriptions.rs): separate
  query refreshes cannot establish a coherent multi-query publication round.
- [store.rs](../crates/hulladb_postgres/src/managed/store.rs): existing receipts are
  tied to successful commit and preserve input for authorization. Lookup-only outcome
  reporting, bounded expiry and terminal-failure fencing need a new contract.

## First gate: prove guarantees before broad implementation

Start with three independently owned proof packets. The orchestrator reviews their
shared boundaries before accepting an implementation design. A spike must produce
executable evidence, measured costs and a decision, not a second production API.
Promote useful code/tests into the selected path and remove throwaway alternatives.

### P1 — Request admission, outcomes and bounded retention

Owner: storage/protocol agent. Primary files: managed store/storage, managed
transport and affected native protocol types. Serving integration and both SDK
consumers are now implemented; loaded deployment measurement remains pending.

Design an original request identity whose fixed validity is server-verifiable after
all per-request markers have expired. Decision confirmed 17 October 2026: use a
reusable short-lived server-issued ticket acquired during connection setup, with
proactive SDK renewal for future calls. Do not add a preflight to every mutation.
Measure cold setup and renewal waits as well as normal calls. Bind tickets to source
and verified caller/tenant/authentication scope; first admission fixes each request's
function and input integrity. Define ticket retirement/config transitions and reject conflicting
reuse of a request identity. Renewal must never change an existing invocation's
ticket/identity or resubmit it. Expiry prevents new admission, not timely lookup of
already accepted outcomes. The implemented wire format and current default durations
are documented below; production-load tuning remains open.
Client clocks or unsigned timestamps are not proof. Coordinate admission, duplicate
execution, in-flight work and terminal outcomes so delayed original requests cannot
commit after a confirmed failure was returned.

Committed business changes and their successful outcome must be atomic. Rollback
alone is not a terminal receipt: persist/fence confirmed failure separately while
preventing a concurrent original execution from succeeding. Recovery authorization
must use minimal verified scope metadata rather than rerunning handlers/extensions
or retaining inputs indefinitely. The policy settled on 17 October 2026 treats
confirmation as delivery of the original authorized response: database permission
changes after commit do not revoke it. Verify current credentials and original
caller/tenant/authentication scope; changed identity or scope and invalid, expired
or revoked credentials must block delivery. No guard replay or duplicate application
policy is required. Browser-provided scope keys are not proof. ACK may delete full results, not
active ownership or duplicate protection. Retention derives from accepted windows
plus cleanup grace.

Required evidence:

| Scenario | Required result |
| --- | --- |
| Commit succeeds, response drops | One business write; authenticated lookup returns the original result |
| Lookup races delayed initial delivery | Missing receipt is nonterminal; no resubmission and no false failure |
| Original delivery duplicated concurrently | At most one business execution can commit for that identity |
| Confirmed rollback/failure reported, original arrives later | Original is fenced and cannot subsequently commit |
| Server crashes before/after business commit | Outcome recovery never contradicts database commit state |
| ACK lost or replayed | Safe cleanup; never makes the request executable again |
| Active work crosses retention boundary | No active receipt/admission state purged unsafely |
| TTL/config shrinks; clocks differ; expired token replayed | Accepted protections survive; expired identity cannot execute anew |
| Identity/tenant/authentication scope changes or invalid credentials | No outcome disclosed across the original authenticated boundary; no guard execution on lookup |
| Database permissions change after commit | Same verified caller/scope can receive the original authorized response within its retained confirmation window |

Report bytes per active/completed invocation, retained row estimates from measured
sizes, write/cleanup overhead and behavior after prolonged disconnection. Do not
claim a result can always be recovered after an unbounded network partition.

### P2 — Coherent query rounds and stable live pagination

Owner: reactive engine agent. Primary files: query execution and managed subscriptions.
Coordinate store changes with P1; do not have two agents edit `store.rs` concurrently.

Prototype a bounded shared-snapshot round, or prove an equivalent validity scheme.
Each published successful current result must correspond to the same database
state. A revision tag on independent reads is insufficient. Include permission
dependencies and empty/failed reads; table-wide invalidation is an acceptable
correctness-first granularity if measured, rather than prematurely inventing a
fine-grained dependency engine.

One SQL error must not poison successful siblings in a shared round: evaluate
savepoint isolation or separate transactions using an exported snapshot. Distinguish
ordinary handler errors from infrastructure failure that discards a round. Ensure
continuous writes do not cause endless restart/starvation. Capture membership,
exclude idle retained queries, fence auth/source changes and apply a complete batch
before notifying observers. Test framework notification behavior too.

For live pages, prove stable ranges, cursor binding, tie-break ordering and row moves
between loaded ranges. Bound rows, bytes, active pages and snapshot lifetime without
silent truncation, skipped records or falsely reporting exhaustion. The policy
settled on 17 October 2026 makes refresh overflow an explicit limit error for the
whole affected paginated list: clear data by default, or preserve the last coherent
list as stale under `keepPreviousDataOnError`. Retain dependency tracking so relevant
database changes recover the list when it fits again. Auth failures override stale
retention. Do not split pages or shift boundaries automatically; ordinary append
failures still retain successfully loaded pages.

Required scenarios: dependent count/list reads during writes; membership revocation;
empty query becoming populated; error before/after partial dependency discovery;
one handler SQL error alongside success; subscriber added/removed mid-round;
disconnect before full batch; sustained writes; ordered row crossing page boundary;
page growth past budgets; repeated/overlapping `next`; append failure after partial
success; filters/auth changing during loading. Report latency, memory and work versus
observed-query/page count, including slow-query head-of-line blocking.

### P3 — Type/API fit and restricted execution boundary

Owner: SDK/runtime agent. Primary files: declaration/type fixtures and worker design;
no conflicting edits to production shared protocol until contracts are reviewed.

Build a small consumer fixture using the exact accepted declarations, generated
imports, Standard Schema extension stages, read/write capabilities, JSON paths,
conflict targets and terminal builder states. Check type inference and editor cost
on circular references and realistic schemas. Standard Schema has no generic schema
AST: do not rely on introspection for input routing or invent result contracts that
the generator cannot derive. Include explicit output transforms and Date transport.

Prove how declared execution restricts network, filesystem, ambient nondeterminism
and imported dependencies. A linter, context interface or an advisory comment is
insufficient for the agreed runtime boundary. Evaluate isolation before committing
to a worker architecture. Confirm execution-deadline enforcement stops precommit work
and prevents a later commit, including output-validator failure and scope escape.
Once COMMIT may have begun, timeout must preserve commit uncertainty rather than
claim rollback. Query abort guarantees local settlement/resource release; cancellation
of already-running server work remains best-effort. It is not mutation cancellation.

Produce executable bootstrap proposals for server-only and framework-free browser
usage. Resolve exact config/mount/verifier/import spelling from those fixtures.
Do not create a framework×auth-provider matrix or reintroduce `createClient`, actions,
internal functions or `db.run`. If runtime enforcement cannot support the accepted
handler semantics, report that limitation before implementation, not in final docs.

## Ordered implementation packets

Each packet includes its dependent fixtures, focused checks, applicable guide edits
and removal of the superseded path. An API is not done when its type signature exists.

| Packet | Deliverable and ownership | Dependencies / exit evidence |
| --- | --- | --- |
| W0 Contract freeze | Orchestrator integrates P1–P3: statement AST, schema/wire values, errors/outcomes, auth boundary, batch envelope and generated imports | Proofs reviewed; unresolved behavior explicit; two tiny executable setup fixtures |
| W1 Storage primitives | Storage agent: canonical schema, parameterized statement execution, constraints/codecs, direct scopes, rollback and OCC defaults; stable-range ordering/cursor execution for W5 | W0; realistic Postgres behavior tests; query/write capability and escape guards |
| W2 Authoring and generation | SDK agent: Standard Schema declarations/extensions, generator entrypoints, builder types, worker bridge and safe browser output; typed pagination results and generated eligibility | W0 + W1 contracts; generated fixtures and Rust manifest consumers updated together |
| W3 First application slice | Orchestrator integrates W1/W2 with supplied service boot and verifier; one schema, real server CRUD and browser query/mutation | End-to-end persisted results; direct server writes invalidate browser reads; no copied host plumbing |
| W4 Confirmation and handles | Protocol owner + client owner sequentially integrate P1 with pending/recovering/success/error, lookup-only delivery, scoped in-memory cache | W0/P1/W3; response-loss, deadline, auth-race and no-resubmit evidence; remove persisted recovery |
| W5 Coherent live results | Reactive owner + client owner integrate P2, atomic cache publication, recovery-on-change and full live-list lifecycle | P2/W4 plus W1 range/cursor execution and W2 pagination recognition; coherent rounds and page/failure/bounds tests; cleanup/resource evidence |
| W6 Optimistic wrappers | Client owner: variadic target descriptors, pure layers/rebase, missing-cache skip, interruption rollback and minimal diagnostics | W4/W5; overlapping writes, updater exceptions and commit/publication race tests |
| W7 Full accepted data surface | Storage/SDK owners complete remaining schema, filters, projections, conflicts, aggregates, codecs and pagination features | W1/W2; explicit accepted-feature checklist, type/runtime/SQL parity; no accidental v2 additions |
| W8 Integrations and learning paths | Adapter/docs owners: thin UI bindings, auth reference storage integration, server/meta-framework recipes and executable guides; deployed Node/Bun application proof with separate realtime service and managed PostgreSQL | W3 onward; portable cloud support is a v1 release gate; release claims only for tested combinations, shared runtime, request isolation |
| W9 Cutover and release audit | Orchestrator: retired-code removal, public export audit, cross-language checks, website/reference consistency and measured limits | All accepted v1 packets complete; no legacy runtime path or unproven guarantee |

W4–W7 need not be monolithic merges. Deliver reviewed vertical slices with one owner
for shared contract files. W7 features should move into W1/W2 when prerequisite to
the starter or proofs. Documentation and deletion work accompany each packet rather
than accumulating until W9. W8 planning and example scaffolding can begin at W0;
framework code waits for the core lifecycle to stabilize.

## Agent coordination and change ownership

Use 6.1-sol for implementation and correctness-heavy packets; luna is suitable for
bounded mechanical cleanup after the contract is frozen. The orchestrator retains
architecture ownership, integration review and final verification. At most three
worker agents run beside the orchestrator. Parallelize independent reads/proofs and
disjoint edits, not two owners of the same shared file.

Every task brief contains: the agreed architecture and relevant detailed decisions;
exact owned paths and consumers; prerequisite contract revision; behavior to deliver;
old code to remove; explicit exclusions; meaningful failure scenarios; and targeted
checks. Agent reports must say what changed, what was removed, what actually ran and
which risks remain. Changes to a shared contract are reviewed before dependent work
continues. Do not accept a local workaround that leaves consumers on a second API.

Shared ownership hotspots include `contracts.ts`, `schema-contract.ts`, generated
manifests, `hulladb_protocol`, `managed/store.rs`, `worker.ts`, package exports and
Makefile targets. `tests/generate.ts` also writes native schema/server fixtures; those
outputs belong to the same codegen packet. Use existing meaningful failure tests
instead of generating tests that merely mirror implementation.

## Removal map

| Retired surface | Remove/migrate with |
| --- | --- |
| Public `HttpRuntime`, `ConnectionSession`, `Predictor` setup and factory-heavy declaration plumbing | W2/W3 generated setup; useful private transport code may remain private |
| Positional validators, `Stored` wrappers, explicit public row-version CRUD, `.watch`, old runtime errors | W1/W2/W4 and all public/generated/native fixture consumers |
| `persistence.ts`, IndexedDB exports, `recoverPending`, reload recovery and payload resubmission | W4 with HTTP/browser/deployed tests rewritten for the new contract |
| Old prediction modes, dependency lists, unknown-outcome retention and opt-out flags | W6 with layer/rebase failure scenarios retained |
| Server identity binding around declaration calls | W3 direct CRUD and explicit mounted verifier; preserve useful request-isolation tests |
| Task-specific replica/sync/protocol/host application path | Retired with its Cargo dependencies, fixtures, examples, Make targets and benchmarks; generic HTTP/TLS transport and security regressions retained |
| Old introductory guides and target-contradicting examples | Corresponding implementation packet and W8; remove from default navigation when superseded |

`hulladb_client` now contains only the reusable HTTP/SSE connector consumed by
`hulladb` remote clients. Keep its TLS and transport regression coverage. CDC diagnostics and optional durable outbox internals
are not automatically browser replica or a new actions platform; retain only where
the accepted implementation or a deliberate diagnostic consumer needs them.

## Documentation workstream

Verified website checkout: `/Users/samuel/Coding/hulla/hulla.dev`. Read its repository
guidance and `skills/write-hulla-docs/SKILL.md` before edits. Its current
`scripts/sync-api-docs.mjs` maps `@hulla/api` only; DB pages are currently curated.
Do not assume a DB sync pipeline exists or overwrite curated pages with that importer.

Current problems are concrete: `db/quick-start.mdx` stops at schema compilation and
`prepareInsert`; `runtime.mdx` requires host/worker construction; navigation follows
implementation layers; the reference mixes browser/server imports; there is no DB
equivalent of the site's executable API snippet check. Fix the learning path rather
than polishing these examples around an unchanged runtime.

Target navigation:

1. Introduction and choice of server-only or browser-reactive use.
2. Server quickstart: schema, actual setup, insert/read, grouped writes and rollback.
3. Browser quickstart: same schema, declarations, supplied service, generated imports,
   rendered states, a mutation visibly changing the query, and subscription cleanup.
4. Schemas and validation, followed by database reads/writes.
5. Transactions and reusable helpers; queries/mutations and authorization.
6. Live results, pagination, failure handling and optimistic updates.
7. Focused integration/deployment guides and an exact reference.

Native Rust, operators, wire protocols, diagnostic CDC and implementation fixtures
stay outside the default TypeScript journey. Preserve useful native capabilities,
but do not require readers to learn two authoring models simultaneously. Use exact
package names (`@hulla/db`, `hulladb`, or the `hulla` CLI), not “Hulla” as a product.

Package guides should own runnable implemented workflows; the website owns curated
introductions/navigation and derives selected reference content only through an
explicit mapping if added. Keep this target architecture internal. Never publish
unimplemented syntax or invent CLI/bootstrap commands to make examples look complete.
Use the actual source files displayed by guides as executable fixtures, not similar
handwritten tests. Review desktop/mobile rendering with the existing docs components.

Documentation acceptance: a new reader reaches persisted data on both paths; every
import and option exists; auth and external-service responsibilities are explicit;
server code is absent from browser bundles; failures and cleanup match the runtime;
advanced internals never block the first result. A release cannot claim an integration
works merely because a core library is framework-neutral.

## Validation and release evidence

Select checks from the actual diff and plausible failure modes per `AGENTS.md`.
Examples of relevant existing targets, to filter and adapt as code changes:

- TS declarations/builders/codegen: affected `packages/db/tests/*.test.ts`, generated
  fixtures and package typecheck; browser bundle boundary checks.
- Storage/transactions/recovery: focused
  `cargo test -p hulladb_postgres --test managed_functions --locked <filter> -- --ignored`.
- Cross-language worker contracts: `typescript_functions` and `typescript_contracts`
  with the `typescript` feature and the corresponding managed-host test. Use the
  existing Makefile target as a reference, not a requirement to rerun unrelated work.
- Transport/subscriptions/auth: focused `hulladb_server` `managed_host` tests, with
  ignored Postgres cases explicitly enabled when relevant.
- Native generic client changes: affected `hulladb` remote and `hulladb_client` HTTP
  consumers; do not silently abandon them when changing the shared protocol.
- Website changes: changed-file formatting, `bun test tests/docs.test.ts`, actual DB
  snippet validation once added, and `bun run build` for MDX/rendering changes.
- Cloud compatibility: deploy a representative application with SSR, an API route
  and a server action using generated direct database access. Verify browser live
  updates from those writes through the separately deployed service, authentication,
  transaction rollback, concurrent request isolation and deployment asset tracing.
  Record the framework, runtime, database endpoint and deployment configuration;
  local emulation or a successful build alone is not production deployment evidence.

Ordinary Cargo runs skip ignored PostgreSQL tests. Report skipped/unavailable checks
as such. Do not run full workspace, CDC or performance suites for prose edits. A final
cross-cutting protocol/workspace removal can justify broad checks once the affected
tests pass; state that reason and avoid repeats without new changes.

Before release, measure extension/validation overhead, normal read/write latency,
OCC contention, receipt storage/cleanup cost, active-query round latency, page growth,
browser memory and typechecking time. Choose remaining numeric limits/backoffs using
those results. No “instant” or bounded-storage claim without corresponding evidence.

## Planning status

- Completed: consolidated accepted architecture, implementation inventory, correctness
  proof requirements, website impact assessment, dependency order and task ownership.
- First implementation slice landed locally: observable awaitable operation handles,
  subscribe, readonly query results, identity fencing, variadic withOptimistic wrappers,
  one DatabaseError class, lookup-only in-memory confirmation with an overall deadline,
  removal of browser persistence/manual recovery/mutation modes, default declared OCC,
  explicit native direct-transaction retry options and confirmed rollback before replay.
  This changes existing code; it does not implement the full target architecture.
- Authoring slice implemented locally: generated schema-bound constructors, object
  declarations and callback shorthand, Standard Schema input/output transforms,
  ordered extensions, original-input isolation, collision rejection, and worker
  output validation before result acceptance. The generator preserves raw caller
  types and transformed result types; it does not inspect validation-library ASTs.
- SDK generation/typechecking and all 77 tests passed, including output-rejection
  rollback in the SQLite test fixture and the real TypeScript worker protocol.
  Real Chromium reload checks passed for the client lifecycle slice. Focused native
  non-database tests/lints, generated-schema normalization and shared wire-contract
  checks passed. PostgreSQL integration coverage is now executed, as detailed below.
- Still pending: production integration of P1 admission/failure fencing/expiry, P2
  coordinated delivery/live pagination and P3 restricted execution; the builder/schema
  replacement, declaration config/limit wiring, simplified bootstrap, framework/auth
  integrations and executable new quickstarts.
  Local proof measurements are recorded below; no production performance claim or
  website update has been made. Review proof outputs before claiming the
  corresponding new runtime guarantees.
- Bring back only concrete tradeoffs that cannot satisfy the accepted contract:
  bounded page growth semantics, runtime isolation limitations, recovery authorization
  after permission changes, or an admission design that materially changes latency/setup.
  Routine naming and internal implementation
  choices stay with the orchestrator and are validated in the setup examples.

## Executed proof gate — 16 October 2026

The orchestrator built checksum-verified PostgreSQL 18.6 from official sources in
an isolated temporary directory because the installed container application could
not start. Tests used a separate loopback port and disposable schemas, without
changing existing databases or installed services. The temporary server was stopped
after validation.

Previously blocked integration checks now pass: managed functions (23), direct
admin transactions (2), typed Rust functions (5), TypeScript workers (4), and the
managed HTTP/SSE host (5, including both generated-SDK end-to-end paths). These
checks validate the current implementation, not the still-unintegrated protocols.

### P1: reusable ticket admission and terminal outcomes

The serving implementation now lives in
[admission.rs](../crates/hulladb_postgres/src/managed/admission.rs), with production
regressions in
[production_operations.rs](../crates/hulladb_postgres/tests/production_operations.rs).
The separate `request_admission.rs` protocol prototype has been removed. Session
setup issues opaque reusable tickets; browser renewal affects future invocations
only. Mutations and confirmation always carry the invocation's original ticket.

First admission fixes ownership, operation, input fingerprint and accepted deadlines.
Business changes and successful outcomes commit together. Failure finalization waits
for rollback and the same request lock; a concurrent committed success wins. Lookup
never registers or resubmits a mutation and never replays authorization guards.

The host automatically drains bounded cleanup batches, skipping active request locks,
expiring result payloads, fencing abandoned pending work, and permanently retiring
issuing tickets before marker purge. ACK releases results without releasing ownership.
Tickets and admission records count against receipt quota, with capacity reserved to
record terminal failure. Accepted replay is not rejected because quotas later shrink.

Current defaults: tickets last five minutes, browser renewal starts at four minutes,
accepted work has a 15-second start window, results retain 60 seconds after completion,
and compact ownership markers have 30 seconds of grace after their protected windows.
These are frozen server-assigned deadlines, not client timestamps or audit retention.
The browser's total confirmation budget remains 30 seconds. The native Rust consumer
renews on the next new invocation after its renewal threshold, persists original ticket
binding, and now uses lookup-only recovery; its older SQLite journal remains until W4.

End-to-end verification covers the shared HTTP host, generated TypeScript SDK, native
remote client and application launchers under Node and Bun. Local timing and storage
samples are recorded in the latest milestone below. Loaded deployment costs and full
Vercel application validation remain separate work; local success does not establish
those claims. No additional app-authored endpoints, ticket switches or public SDK
recovery methods were added.

### P2: coherent reads and stable range primitives

[query_round.rs](../crates/hulladb_postgres/src/managed/query_round.rs) evaluates a
captured set in one bounded read-only repeatable-read transaction. Per-member
savepoints isolate ordinary SQL failures; infrastructure failure, global deadline
or result-budget overflow discards the round. Failed handlers conservatively track
all managed tables. Six integration cases cover concurrent writes, authorization,
empty/failed reads, SQL failure, deadlines, connection loss and sustained writes.

[pagination.rs](../crates/hulladb_postgres/src/managed/pagination.rs) initially proved
fixed tuple boundaries, key tie-breaking, row moves, deletions, empty ranges, cursor
binding and explicit growth/byte overflow. The integrated pagination packet below
replaces its restricted order/filter representation with the shared predicate and
ordering model, including nullable/multiple fields and public opaque cursors.
Its physical-row JSON budget remains separate from the SDK wire budget.

Local rounds of 1/8/32 small members took about 0.77/3.42/10.91 ms. A 10 ms slow
member delayed its sibling in a 13.88 ms round. Twenty rounds during twenty writes
finished in about 30.53 ms without incoherent count/list pairs or restarts. Fixed
page ranges grew from two to four rows; a three-row budget correctly rejected the
refresh instead of truncating or claiming exhaustion. On 17 October 2026 the user
selected the existing query error policy for refresh overflow, applied to the whole
paginated list, with automatic recovery on relevant changes and no automatic range
splitting. The integrated pagination packet below implements that client policy.
Retaining a caller's prior test value alone is not proof of client cache publication
or framework notification behavior.

The 19 October W5 delivery packet below integrates atomic ordinary-query batches,
membership/auth fencing and client notification. The later pagination packet extends
that delivery path with stable-page append/disposal and whole-list error handling.

### P3: restricted execution feasibility

The initial temporary QuickJS proof established feasibility with 23 scenarios and
Zod async transforms. It has been replaced by the maintained native engine and
PostgreSQL/service integration tests described in the 19 October milestone below;
the duplicate proof implementation has been removed. Its simulator timing samples
were not production benchmarks and do not characterize the integrated engine.

Decision confirmed 17 October 2026: accept restricted JavaScript package compatibility
for v1 declarations. Bundle compatible computation/validation dependencies; external
I/O, framework server APIs, Node/Bun libraries and native dependencies stay in ordinary
application server code. Async validators and imported helpers receive no additional
capabilities. Generation should reject known unsupported imports with actionable
diagnostics, backed by runtime enforcement. Add representative package conformance
coverage; do not infer universal npm support from the Zod proof. Engine selection
remains internal, subject to the integration and packaging evidence above.

Decision confirmed 17 October 2026: configuration uses a conventional schema path
(`./db/schema.ts`, overridable through `schema`), with recursive function discovery
beside that schema by default. `functions` is an optional directory override, not
an entrypoint. Recognize exported query/mutation declarations and `.extend`-derived
declarations statically; ordinary helpers do not become endpoints. Exclude schema,
config, generated files, tests and dependencies from discovery roots, while retaining
normal imported-helper dependency analysis. No whole-project module execution to
discover declarations or registry file. Generation does not start a network listener. The setup
fixtures must exercise omitted paths and custom schema/function locations.

Decision confirmed 18 October 2026: keep one authored declaration and disposable
generated client bindings, with no duplicate handlers in browser bundles or manual
re-export registry. Present remote calls as typed RPC; the supplied service and SDK
handle networking, verification, live subscriptions and confirmation. Keep HTTP/SSE
as the implementation direction without making transport setup an application task.
No service worker or direct-source-import bundler plugin is required for v1.

Decision confirmed 18 October 2026: drop both proposed `browser: true` and `api: true`
switches. A declared query/mutation is already an explicit remotely callable operation;
the running Hulla service automatically serves discovered declarations. Server-only
applications omit declarations and use direct operations/helpers. Importing modules
or generating bindings never starts a listener. Helpers and direct server operations
are never published as function endpoints. Test those boundaries in setup fixtures.

Clarification confirmed 18 October 2026: preserve the settled auth ownership boundary.
The proposed mandatory `{ identity, expiresAt }` verifier envelope is not accepted.
Auth providers own expiration/refresh/revocation; browser subscriptions update Hulla
auth metadata, and server credentials are verified through the trusted integration.
Do not make authors duplicate session expiration metadata or lifecycle management.
The auth package/application calls `setAuth(null)` on expiry or sign-out. Hulla's
client responsibility is to retire old auth-scoped state and fence late results;
the original decision omitted local expiry timers (superseded by the optional
client deadline accepted on 19 October below). No credential refresh flow. Include
an application-driven expiry fixture exercising this existing invalidation path.
Prove that live delivery revalidates through that integration or reacts to trusted
invalidation signals, including expiry/revocation with an unchanged identity key.
Do not mistake client-only notifications or a cached identity for server validity.

Recovery-result authorization, live-range overflow behavior, reusable admission
tickets, restricted package compatibility, default discovery and generated RPC
bindings and automatic serving of declarations are settled. The credential ownership
boundary is also settled: application/provider lifecycle, separate credential
transport, and trusted server verification. Implement deployment/connection setup
and executable setup fixtures within those boundaries; do not reopen them as
additional enablement or expiry options. Admission timing/retention defaults still
need measurement and protocol validation. Do not claim completion of the full
accepted API before the remaining integration is implemented and validated. The
orchestrator owns documentation and the user-facing decisions; agents implement
code and review explicit work packets.

### Implemented 18 October 2026: auth retirement and declaration discovery

The TypeScript client no longer requires `ConnectionSession` or `setSession`.
Generated runtimes export `setAuth`; the application supplies client identity
metadata, with `null` on expiry/sign-out. Credentials travel separately through
cookies or a current-header provider. The server supplies verified context during
the handshake; browser metadata cannot establish identity. Same-state notifications
preserve results, while changed identity or expiry retires cached data, optimistic
layers, completed and pending handles, and active subscriptions synchronously.
Late credentials/results and reentrant observers cannot restore the retired scope.

The managed host verifier now receives full original request headers, permitting
application-owned cookie/bearer schemes and explicit anonymous acceptance. Live
checks reverify through that integration, including revocation/claim changes with
an unchanged principal. The browser can reconnect using provider-refreshed
credentials only after confirming the same verified scope. No SDK token refresh,
expiration metadata, identity inference or mutation resubmission was added.

Static discovery now scans beside the schema by default, supports an optional
functions-directory override, recognizes real declaration brands and extension
builders, and excludes helpers/config/tests/generated outputs. Nested module paths
are retained. Discovery executes no candidates; subsequent generation still evaluates
selected trusted declaration modules to emit their metadata. That current build
boundary is documented rather than presented as restricted runtime execution.

Validation: package generation, typechecking and all 94 SDK tests passed (730
assertions). Real PostgreSQL managed-host tests passed for anonymous/cookie access,
invalid credentials, spoofed context pins, live revocation and context changes;
the native remote identity-pinning/SSE integration also passed. Changed Rust targets
passed formatting and clippy. Both generated-SDK PostgreSQL HTTP/SSE end-to-end tests
passed, including execution through the TypeScript worker and receipt confirmation.
Real Chromium passed the main lifecycle checks and actual `?reload` navigation:
app-driven sign-out clears completed mutation/live-query observers, missing credentials
are rejected by the server, and reload performs a fresh authoritative read with
lossless int64 results. The browser harness uses a two-second confirmation window
to tolerate cold startup and still exercises terminal timeout. Final fixture
typechecking passed; the disposable PostgreSQL and browser servers were stopped.

Remaining implementation includes simplified config/service bootstrap and generated
server database operations, production integration of restricted handlers and the new
admission/terminal-result protocol, coordinated multi-query publication, and remaining
schema/query-builder surfaces. The website still needs its planned guide/navigation
rewrite; the package README and adapter guide now describe the implemented auth and
discovery boundary. This packet does not complete those larger workstreams.


### Implemented 19 October 2026: optional client expiry hint

Authenticated `setAuth` objects now optionally accept `expiresAt`, an integer Unix
timestamp in milliseconds. Omission retains application-driven invalidation. A
provided deadline retires local state through the existing auth boundary, with
access/delivery checks protecting suspended tabs. Refreshing or removing a deadline
for an unexpired identity preserves its cache. Past deadlines clear immediately;
invalid values fail before modifying state. Disposal cancels the timer, and long
deadlines are scheduled without timer overflow. This updates the earlier no-timer
client decision; provider-owned credentials and server verification are unchanged.

Focused HTTP/handle tests and TypeScript validation cover expiry, renewal, omission,
invalid values, late results and delayed timers. No Rust/server protocol changes
are needed for this client-only hint.


### Implemented 19 October 2026: inferred auth state

Removed caller-supplied status fields. `setAuth` and the development adapter's initial
`auth` accept `{ key, expiresAt? }`, `null`, or a promise resolving to either.
Pending resolution retires the old scope and blocks requests. Only the latest update
can install state, including when newer updates occur synchronously in observers.
Rejection/invalid metadata blocks access until another update and rejects the
completion promise. Superseded resolutions and post-disposal completions are ignored.
Synchronous updates retain the settled expiry and same-identity cache behavior.

Validation: 63 focused HTTP, handle and framework tests passed (552 assertions),
including promise races, old expiry timers, rejection, invalid metadata and disposal.
Package TypeScript checking passed, including migrated browser and deployed fixtures.

### Implemented 19 October 2026: generated server database and explicit setup

`defineConfig` separates connection settings from the schema. `hulla-db generate`
discovers declarations and emits schema-bound functions, types, browser bindings and
the server database; `hulla-db init` explicitly initializes storage. Imports and
generation open no database connection or listener. Repeated initialization preserves
matching data; incompatible schemas fail without reset. Native binary distribution
is not yet packaged, so the [server setup guide](server-database.md) documents the
actual repository development commands.

The public database now uses lazy awaitable builders and plain rows. Single/bulk
insert, equality predicates, mandatory predicates for update/delete, limit and unique
are implemented. Repeated awaits reuse their result. Undefined patch fields remain
unchanged, unknown properties fail, and caller-supplied concurrency versions are gone.
Existing worker consumers use the same operation decoder and row representation.

Generated server calls use a private persistent native process and lazy connection
pool. Reads use a snapshot; writes use serializable PostgreSQL transactions and share
the revision machinery observed by subscriptions. Direct operations create no
mutation receipts. Transactions run once by default. Explicit retries replay only
after confirmed serialization rollback. Malformed or lost commit acknowledgments
preserve uncertainty and never cause replay. Root access inside a transaction, escaped
transaction handles and started but unawaited operations fail; unrelated requests
remain independent. SQL errors poison the transaction even if caught by the callback.

Validation: 108 SDK tests passed (852 assertions) after the shared builder/generation
changes; an additional focused protocol test passed for malformed, oversized and
unexpected commit acknowledgments. Final TypeScript checking passed. Three direct
bridge PostgreSQL integration tests passed, including actual generated setup/CLI,
repeatable initialization, rollback, asynchronous scope isolation, 1,001-row reads,
large limits, confirmed conflict retries and absence of receipts. Three affected
TypeScript-worker PostgreSQL tests and the managed-host live-subscription test passed;
the latter verifies direct writes refresh existing subscribers. Focused Rust clippy
and formatting passed. No new browser run or performance benchmark was claimed.

This completes the initial direct-server milestone, not the full replacement. Next
implementation work is the supplied browser-service bootstrap, production restricted
handler integration and admission/terminal-result protocol, coherent live publication,
the remaining schema/filter/projection/aggregate/pagination surface, framework adapters,
configurable runtime limits and native distribution. Legacy cleanup and the website
guide/navigation rewrite remain necessary before release. Do not reopen settled API
decisions merely to add setup switches.

### Implemented 19 October 2026: supplied browser-service startup

`hulla-db serve` now generates discovered declarations, verifies existing storage
and launches the existing native HTTP/SSE host. `_generated/worker.ts` imports the
same discovered declarations as the browser manifest; applications maintain no
worker entrypoint or registration list. Startup never initializes storage. Generated
browser `configure({ url, headers?, credentials?, auth? })` owns the runtime and returns
an asynchronous teardown function. Imports and configuration remain connection-free
until a call/subscription starts.

Config accepts listener/origin settings and an optional provider verifier receiving
original headers. It returns a stable key plus JSON identity or null for anonymous
access; invalid credentials throw. The private parent/native protocol derives context
pins from verified account/claims and repeats verification for live delivery. Client
cache metadata cannot supply those pins. Deadline/capacity failures are unavailable,
not invalid credentials, and timed-out verifier promises cannot permanently occupy
capacity. Generated handlers infer nullable identity via type-only config imports;
provider implementation and connection secrets are absent from generated workers and
browser bundles. Direct database calls retain application-owned authorization.

Validation: generation/typechecking and all 116 SDK tests passed (914 assertions).
Focused auth tests passed after the final timeout classification change. The real
PostgreSQL service integration exercises CLI generation/init/serve, generated browser
configuration, anonymous and authenticated calls, forged client cache keys, credential
rejection, provider timeout/recovery, CORS, direct server writes refreshing live queries,
revocation, account changes and shutdown. Native protocol failure tests, focused clippy
and formatting passed. No real-browser rerun or performance measurement is claimed.

The [service guide](browser-service.md) and existing setup/readme guides now document
the working development path. The website rewrite remains pending. Production
restricted handlers, admission/terminal results, coordinated query publication,
the remaining query/schema surface, framework adapters, configurable budgets, native
distribution and legacy cleanup remain unfinished. This packet introduces no actions,
internal-function markings, auth-provider implementation or framework combination matrix.

### Implemented 19 October 2026: restricted native handler execution

Replaced the trusted Bun handler process with `hulladb-js`, using pinned rquickjs
0.14.0 and its QuickJS-NG 0.16.2 engine. The service generates and bundles discovered
declarations automatically. Pure declarations are separate from direct-server imports,
so schema-bound constructors do not pull host APIs into their bundle. There is no
fallback Bun executor. The temporary feasibility host was removed; the maintained
engine and actual PostgreSQL integration now own that coverage.

Each invocation compiles trusted generated source separately and executes it in a
fresh realm without dynamic compilation, a module loader, network, filesystem,
process access or ambient clock/random. Bundled helpers share these restrictions.
Native code retains transaction ownership, readonly enforcement, operation sequencing,
precommit checks and cancellation. Pending work, detached failures and writes attempted
during result serialization cannot publish success. Engine deadlines are reported as
transaction deadlines, including when the supervising process has a longer budget.

Generation rejects known unsupported imports before evaluating selected declaration
modules. Narrow import interception plus build metadata inspection avoids a Bun 1.4
namespace-reexport bug uncovered by real Zod validation. This is covered by regression
tests rather than dependency downgrades or output rewriting. Config/schema evaluation
remain trusted build steps. No arbitrary npm compatibility or independent engine
security audit is claimed.

Validation: all 122 SDK tests passed (927 assertions), with generation and TypeScript
checking. Eight native engine tests cover host/compiler denial, deterministic date
operations, correlated database messages, unfinished work, sanitized failures, CPU
and regex deadlines, memory and stack exhaustion. Four PostgreSQL handler cases
passed, including staged-write rollback on the engine's own deadline. The generated
HTTP/SSE integration passed, as did the full CLI service setup with auth changes,
live direct-server writes, real Zod 4.6.7 and Valibot 1.5.0 async validation/transforms,
and output-validation rollback after a write. Focused Rust linting and formatting
passed. No production performance benchmark or real-browser rerun is claimed.

Current bounds are five seconds, 64 MiB of interpreter heap, 512 KiB stack, 16 MiB
bundles/frames and the native operation budget. Their configuration remains unfinished.
The next implementation packet is admission and terminal-result confirmation; coherent
multi-query publication, remaining schema/query features, framework adapters, native
distribution, legacy cleanup and the website rewrite also remain. Do not reopen the
settled public model to add runtime selectors or new application concepts.


### Implemented 19 October 2026: Bun tooling and portable service startup

Separated `dev` from `serve`. Bun owns generation/bundling and the development
supervisor. `dev` regenerates in a fresh child process after source changes, stops
the old child before replacement, ignores generated/framework/build/log output and
continues watching after invalid edits. Configured external schema/function roots
are watched and reconciled when configuration changes; imports outside those roots
and external symlink targets still require manual restart.

`serve` loads existing worker/manifest artifacts without importing the generator,
performing discovery or writing generated output. Missing artifacts fail with a
build instruction before native startup. Configuration and the auth verifier run in
the chosen application runtime. Source-checkout commands support Node 22.18 and Bun;
the Bun CLI entrypoint remains the development default. Published JavaScript/native
packaging is a separate remaining task, not implied by source-checkout compatibility.

The runtime implementation uses Node-compatible APIs. Replaced TypeScript parameter
properties with equivalent ordinary fields so Node's native TypeScript loader can
execute source-checkout consumers. Generation continues to require Bun even when
production runs under Node. The restricted handler engine and native database service
remain identical across those application runtimes.

Validation: actual direct-server and complete service PostgreSQL integrations passed
under both Node 22.18 and Bun 1.4. The service tests keep generated output read-only,
make authored handlers throw if evaluated, and verify unchanged artifact hashes.
Focused schema/database/service tests passed (20 tests), plus the subprocess watcher
lifecycle test and a discovery regression: syntactically invalid source now fails
before stale-output cleanup, preserving existing bindings. Watcher tests require
native filesystem notifications outside the sandbox. TypeScript checking, focused
Rust linting/formatting and diff review passed. No production speedup benchmark or
new real-browser validation is claimed.

### Accepted 19 October 2026: portable cloud deployment proof

Required topology: an application host supporting Node/Bun and native subprocesses,
a persistent `@hulla/db` service, and shared PostgreSQL. These may run on containers,
virtual machines or suitable function hosts. Next/Vercel is one compatibility fixture,
not the architecture or an exclusive deployment target. Managed PostgreSQL support must be
tested with an actual provider endpoint; PostgreSQL protocol compatibility alone
does not establish pooled-endpoint or connection-lifecycle compatibility.

Initial gaps when this packet was accepted (implementation progress is recorded below):

- The SDK exports source TypeScript and resolves its native bridge to a checkout's
  `target/debug` binary unless overridden. Production JavaScript/native distribution,
  deployable Linux assets and framework file tracing remain unfinished.
- Direct database access lazily pools native bridge processes. Idle processes have
  no idle drain policy. `db.close()` closes the entire shared handle, so calling it
  after each request is unsafe when concurrent requests share a warm instance.
- Local tests prove Node/Bun execution and PostgreSQL semantics, but do not exercise
  function suspension, instance retirement, platform termination or deployed native
  binary loading. Existing statement/lock timeouts do not alone establish cleanup
  of a suspended callback holding an open transaction.
- No executable Vercel application fixture or deployed compatibility evidence exists.

Implement and validate this packet before claiming v1 deployment support:

1. Package the production SDK and native bridge, then build a small Next.js example
   using the existing generated API. Verify native assets survive file tracing and
   run from the deployed filesystem without Cargo or runtime generation.
2. Establish safe process/connection reuse and cleanup under concurrent requests,
   idle instances, stale connections and interrupted transactions. Preserve scope
   isolation and rollback guarantees; never retry an uncertain commit. Avoid
   request-level cleanup that terminates another request's database work.
3. Deploy the persistent service separately and connect a managed PostgreSQL instance.
   Prove SSR reads, route/server-action writes, grouped rollback and authenticated
   browser queries observing committed direct writes. Check the supported direct
   or pooled connection mode explicitly rather than assuming both work.
4. Exercise cold/warm invocations, concurrent requests and connection interruption;
   document platform termination limits and bounded cleanup. Check preview deployment
   configuration isolates database/service/auth settings from production and keeps
   credentials out of browser bundles.
5. Publish the tested recipe and precise support matrix. Keep the same SDK syntax;
   do not add deployment selectors or substitute weaker transaction behavior.

Use Vercel's [Node runtime documentation](https://vercel.com/docs/functions/runtimes/node-js),
[file inclusion guidance](https://vercel.com/kb/guide/how-can-i-use-files-in-serverless-functions)
and [function limits](https://vercel.com/docs/functions/limitations) when implementing
and recording the tested deployment. Recheck these platform constraints at proof time.
This packet records required work; no Vercel deployment test has passed yet.

### Implemented 19 October 2026: original-response confirmation

Committed mutation confirmation now delivers the saved response under the original
principal, data/authentication scope, source and expiry fences. Receipt lookup no
longer needs a registry and cannot execute authorization policy, handler or input/output
validation. Removed the obsolete `Invocation::Recovery` branch and migrated native
consumers and examples. Duplicate submissions check the stored operation/input before
current declaration lookup or validation, including after a deployment changes or
removes the declaration. New executions still use current validation and policy.

The HTTP host continues to verify current credentials before lookup. Regression
coverage proves that revocation and changed authentication scope block delivery,
an anonymous caller cannot obtain another principal's response, and a query-only
replacement registry can deliver the original mutation response. Typed clients
retain their own result decoding; this change does not promise compatibility with
an arbitrarily changed client result contract.

Validation: 35 real PostgreSQL integration cases passed: 23 managed-function cases,
one native TypeScript handler case, two HTTP host cases, four receipt retention/quota/
source cases and five Rust facade cases. These include duplicate races, changed
database permissions, removed/replaced declarations, guard invocation counts,
credential fencing and expired results. Affected Rust consumers compile with all
features; clippy passed for `hulladb_postgres`, `hulladb_server` and `hulladb` across
all targets/features. No broad SDK, browser or deployment validation is implied.
Canonical guides and the website's authentication explanation now describe this
policy. The changed website page passes formatting; all 18 documentation integrity
tests and the production website build passed. The broader website rewrite remains
pending.

The reusable admission-ticket, persisted terminal-failure and bounded marker-purge
work is still pending. This milestone implements the agreed response-delivery policy;
it does not turn an absent receipt into confirmed failure or establish the full P1
protocol. Portable packaging and hosted deployment proof remain required for v1.

### Implemented and proved 19 October 2026: input binding and reusable admission

The serving path now retains a 32-byte SHA-256 input fingerprint in each successful
receipt instead of a second full copy of the submitted JSON. The operation name
remains part of duplicate-request binding. Canonical hashing sorts object keys at
every level, preserves array order and scalar types, and normalizes floating-point
negative zero to preserve existing JSON equality. It does not coerce strings or
numbers. Both ordinary duplicate delivery and serializable-conflict confirmation
check the same fingerprint; compaction removes the result while retaining the binding.

The storage layout check now requires the fingerprint column. Older unpublished
development layouts are rejected; no migrations, automatic reset or compatibility
branch were added. Application data and intentionally returned result payloads are
unaffected. Fingerprints are not an anonymization mechanism.

Replaced the obsolete per-request signed-token/read-certificate proof with reusable
opaque tickets. Eight PostgreSQL cases cover admission/cleanup locking, competing
first payloads, renewal without rebinding an existing invocation, duplicate execution,
failure fencing versus a winning duplicate, actual connection loss before/after
commit, ACK, changed identity, active work, orphan cleanup, frozen retention and
backwards-clock replay after purge. The proof reuses the serving input-binding helper
privately instead of maintaining another hashing implementation.

One local proof sample measured issuance 0.915 ms, renewal 0.446 ms, admission
2.305 ms, execution 1.661 ms and cleanup 2.087 ms. Tuple sizes were 112 bytes per
ticket, 216 pending, 248 completed and 224 after ACK for the small sample result.
These exclude indexes, WAL, dead tuples and larger results. HTTP cold setup/renewal
RTT and loaded behavior were not measured; the fixture's artificial durations do
not establish production defaults or extend the client's confirmation deadline.

Validation: 32 affected serving-path PostgreSQL cases passed (24 managed functions,
five production operations, two outbox and one native TypeScript handler), plus
eight admission proof cases and canonical input equality coverage. The three proof
cases strengthened with concurrent conflicting admission and connection-loss barriers
passed after their final edits. Focused all-target/all-feature PostgreSQL clippy,
formatting and diff checks passed. No full SDK/browser/deployment rerun was needed
for this internal storage change. Package operational guides were updated; no new
browser syntax or website integration claim was introduced.

This earlier proof milestone is superseded by the serving integration below; its
private protocol fixture has been removed after adding production regressions.


### Implemented 19 October 2026: serving admission and bounded outcomes

The production HTTP host, generated browser SDK and native remote consumer now use
reusable tickets throughout mutation submission and lookup-only confirmation. Browser
renewal is proactive while active and never changes an existing invocation's identity.
Native renewal occurs before a future call after the renewal threshold. Both clients
ACK delivered successes, preserve the original ticket across recovery, and distinguish
terminal execution/quota/deadline failures from transient capacity or connection loss.
The browser keeps only its existing in-memory lifecycle; no offline journal was added.

Storage records admission before executing a handler. Success commits atomically with
business writes; a failure finalizer locks the admitted request after rollback and
returns an already committed duplicate's success if necessary. Tests reproduced and
fixed a race where a trusted unticketed serializable execution could commit after a
ticketed failure: first-admission transactions now use serializable isolation with
bounded whole-admission retries. Complementary receipt/admission reads establish the
conflict without another lock table or a new public API. Handler retry policy is
unchanged. Retained success adoption does not extend its original result window.

Admission and tickets count against receipt quota; a pending admission reserves 128
bytes for terminal metadata. Shrinking quota does not prevent delivery of accepted
outcomes. ACK/expiry compacts a result and removes it from the partial expiry index.
Automatic maintenance skips active locks, drains bounded batches within a host time
budget, releases pool slots between batches, and retires a ticket permanently before
purging its compact request markers. This does not implement application history.
The lower-level trusted unticketed storage API continues to keep permanent markers;
HTTP callers cannot select that path.

The standalone request-admission proof is removed. Seventeen production operations
cases now include concurrent original delivery, conflicting first payloads, losing
rollback versus winning duplicate, interrupted partial writes, active work past its
start deadline, quota reservation, early/repeated ACK, retained auth/source ownership,
expiry, purge, and native/HTTP identity collisions. The shared managed-function target
passed 24 cases, outbox seven, and the focused native TypeScript receipt case one.

Seven real PostgreSQL HTTP/SSE host cases passed, including generated SDK calls through
both Rust and TypeScript handlers, response loss, automatic host cleanup, auth fencing,
renewal, and ACK. Both application service launcher integrations passed under Node and
Bun. The native remote target passed 16 local transport cases; its real-host PostgreSQL
case passed separately before the additive execution-limit error mapping. The browser
passed 40 HTTP cases and 35 affected consumer cases. TypeScript checking, relevant Rust
strict Clippy/formatting, and 18 website documentation integrity tests passed.

One local loopback sample measured 6.05 ms cold session setup, 2.89 ms at the renewal
endpoint and 5.36 ms for an admitted Rust mutation (14.59 ms for the TypeScript worker
fixture). These are individual observations, not a benchmark or latency guarantee.
A stored ticket occupied 112 bytes; the acknowledged invocation sample occupied 208
bytes in admission plus 361 bytes in its receipt. Indexes, WAL, dead tuples, result
payloads and database page overhead are additional. Tickets are shared across calls.

Incompatible development layouts are rejected, never silently recreated or migrated.
Package recovery/operations guides and the website recovery explanation were updated.
No deployed Vercel claim is established: cloud deployment proof, remaining schema/query
work, coordinated publication and broader documentation cleanup remain in the plan.


### Implemented 19 October 2026: portable packaging and deployment fixture

Deployment is provider-neutral: one compiled SDK and native runtime for application
hosts that support Node/Bun subprocesses and PostgreSQL, plus a persistent realtime
service. Next is an executable framework compatibility fixture, not a provider
selector or second implementation. No hosted infrastructure has been supplied;
actual deployment and managed-endpoint verification remain pending.

The npm packaging path now emits JavaScript and declarations, validates staged native
executables against their OS/architecture, and includes traceable assets relative to
the installed package. A Node CLI runs prepared services and initialization; generation
and development still use Bun. No Cargo or code generation runs in production. Local
artifacts contain genuine macOS arm64 and Linux x64 release binaries; the Linux build
recipe uses Debian Bookworm/glibc. Other platforms are not inferred to work from these
results. Source-checkout tests use an explicit export condition to select the same
source modules consistently rather than mixing source and compiled schema identities.

Direct database handles retain their four-session lazy pool, drain idle sessions after
ten seconds, and validate reused connections. PostgreSQL enforces thirty-second idle
session and transaction bounds independently of the application event loop. Native
stdin retirement also needs explicit process exit because Tokio's blocking stdin
worker otherwise prevents runtime shutdown. A callback waiting inside a transaction
also expires its native process after thirty seconds; an explicit expiry flag makes
retirement independent of whether PostgreSQL or the native timer acts first. Leased
sessions are excluded from JS idle drain. No request-end db.close, public lifecycle switches or uncertain-commit retries
were introduced.

The installed-package browser fixture exposed a native fetch receiver bug that injected
fetch implementations had hidden. HttpRuntime now binds fetch to the host global;
its JSON and streaming paths share that implementation. The existing HTTP target
passed all 38 cases after the fix. The real browser fixture supplies the regression
coverage, alongside standalone asset tracing, SSR, route/server-action writes and
rollback checks.

Validation passed: Node/Bun direct-server and application service integrations,
generic and TypeScript PostgreSQL HTTP/SSE lifecycles, the real thirty-second idle
and suspended-callback regression, source watcher restarts, five worker cases,
fifteen codegen cases, package typechecking, bridge strict Clippy and formatting.
Installed artifacts execute outside the checkout without Cargo; the browser export
bundles without native/configuration modules. The actual Linux Node container proved
initialization, CRUD, grouped rollback and an authenticated restricted-worker query.
The standalone Next/Chromium proof verifies traced native assets, SSR, route and
server-action writes, live updates, rollback, revocation clearing and absence of
private credentials from static browser assets. The fixture content-addresses local
archives so a package-manager cache cannot silently substitute an earlier tarball.

The canonical deployment, direct-server and service guides now describe portable
requirements and lifecycle limits. The website runtime guide received the matching
deployment explanation; its 18 documentation integrity tests and production build
passed. No package publication, cloud deployment, managed PostgreSQL/pooler validation,
Linux arm64 execution or general v1 completion is claimed. Schema/query completion,
coordinated subscription publication and broader docs cleanup remain separate work.


Final artifact verification: `/tmp/hulla-db-final-proof.tgz`, SHA-256
`561b57032070fe888560c93bde2b0d83bc4372862c8b16636e854d3dbf873bc5`, passed
both the final Linux installed-runtime proof and the full Next/Chromium fixture.
The latter records its artifact hash and checks in `result.json` under the reported
temporary proof directory. All test-owned schemas and containers were cleaned up.

### Implemented 19 October 2026: W5 ordinary-query coordinated delivery

The subscription host now evaluates each captured query set through the P2 shared
repeatable-read round, replacing the single-query subscription wire. One SSE frame
contains every member's result or handler error. Requests reject empty, duplicate-key
or over-128 membership; deadlines and serialized frame limits bound the complete
round. Identical captured sets share refresh work, while every connection retains
independent credential checks and permits. Scope-head polling avoids rerunning failed
handlers without database changes; joins recheck authorization. Empty reads and
failed members recover after scope writes. SQL savepoints remain the storage layer's
primary isolation mechanism; infrastructure failures discard the round.

The TypeScript HTTP owner uses one stream for its actively observed set, rebuilding
membership in a microtask and fencing superseded streams. It validates complete
membership and decodes all results before installing caches and optimistic bases.
Internal notification staging updates sibling query handles before application
listeners run. Await-only responses cannot advance a live member independently;
observed handles wait for the first live round even when their one-shot await resolves
earlier. Optimistic bases install together before predictors rebuild. Handler errors
stay observable and recoverable; forbidden members clear protected data, while
credential/source changes retire the owner. Interrupted or partial frames retain the
last complete view as stale.

The Rust remote consumer sends singleton rounds and validates their complete shape,
cursor, revision and auth/source fences. Its typed watch adapter now permits member
error recovery on the same stream while still closing after terminal stream failure.
It does not claim coherence across separate native subscriptions. No old subscription
wire or compatibility version remains.

Removed the superseded `QueryCache`/`query_cached` storage API, its cache-specific
test/helper and export. Its only production consumer was the replaced subscription
host. Ordinary one-shot queries still execute directly; existing authorization/read
tests and the round tests cover the retained behavior. This removes the parallel
per-query cache policy instead of maintaining two implementations.

Validation passed: focused HTTP/handle/optimism tests and TypeScript checks; six
PostgreSQL host cases (five in the target run, corrected cookie case rerun separately);
18 native remote cases including the real host, plus the shared local-watch regression;
the restricted TypeScript handler/receipt lifecycle; and actual application service
integration under both Node and Bun. Server/native Clippy and formatting passed.
The first unprivileged application integration attempt could not access PostgreSQL;
both cases passed with local database access. Website documentation integrity checks
(18 cases) and the production site build passed after updating the runtime guide.
After removing the obsolete cache, all 23 managed-function and six shared-round
PostgreSQL cases passed, along with affected formatting checks.

Integration review also found that all-error rounds must retain revision coverage.
The parallel resource audit added an epoch-scoped live revision fence, independent
of successful cached results. Its expanded HTTP regression rejects a lower-revision
success after an all-error round and recovers at a later revision; all 44 HTTP cases
passed. That audit also implemented bounded five-minute inactive browser retention
and is separately verifying final dependency/native packaging changes.

Remaining W5 work includes stable live-page integration and its whole-list failure,
append and growth budgets. Broader schema/query features, framework bindings, a hosted
cloud deployment and the remaining website rewrite are still outstanding. The local
P2 measurements above describe storage primitives, not a new end-to-end performance
benchmark; no latency improvement is claimed for this delivery packet.

### Implemented 19 October 2026: W7 scalar filters and deterministic ordering

The shared TypeScript database builder now supports the accepted explicit positional
operators and callback composition (`and`, `or`, `not` and comparison helpers).
Object filters remain equality shorthand; repeated `where` calls AND their complete
conditions. Literal text matching escapes SQL pattern characters and supports an
optional case-sensitivity setting. Explicit `like`/`ilike` preserve SQL patterns.
Null equality normalizes to IS checks, null-containing membership lists match null
explicitly, and ordinary comparisons preserve SQL three-valued logic. Undefined
operands fail. Builders retain lazy, memoized execution and immutable branches.

Schema generation emits `tables.ts` with type-only schema imports, named exports
and a default map for arbitrary table names. Table references work across CRUD;
same-table field references work in comparisons and ordering. Strings in value
positions remain literals. Computed metadata keys preserve names such as `__proto__`;
generated local aliases avoid collisions with table export names. UUID fields still
share string value types in the current schema model, so text-versus-UUID operator
restrictions additionally require runtime validation.

The native statement compiler replaces the equality-only builder wire with a bounded
predicate tree and parameterized PostgreSQL execution. Direct server calls and
restricted handlers use this same decoder. Selections default to primary-key order;
custom ordering appends the primary key in the last direction when absent. The
older native Rust `Select` facade remains a supported independent authoring surface,
not a fallback implementation of the new TypeScript builder.

Primary regression coverage checks SQL null/grouping/escaping semantics, field
comparisons, deterministic ties, writes and rollback in PostgreSQL. SDK cases check
type narrowing, table scope, captured-array immutability, branch execution and frozen
predicate trees. Existing generated-consumer tests now compile table references;
the unusual-name regression imports generated references after removing the authored
schema, proving they do not execute it. Existing direct-server and service fixtures
exercise the full generated path under Node and Bun. This caught and removed a
TypeScript parameter property unsupported by Node's strip-only source loader.

Validation passed: package typechecking and build; focused database, worker and
server-lifecycle tests; codegen/schema tests; the native scalar predicate regression;
the restricted TypeScript CRUD/receipt regression; and four generated Node/Bun
direct-server/service integrations. PostgreSQL all-target Clippy and formatting
passed. The first SDK test invocation omitted the repository's source condition and
resolved stale dist files; the corrected source-conditioned run passed. Website
documentation checks (18 cases) and production build passed after updating functions,
generation and client import paths. These checks do not constitute a new hosted or
Linux installed-package proof.

This is a W7 prerequisite slice, not completion of W7 or W5. The following packet
integrates stable live pagination across the statement/handler/client layers. JSON and array schema values,
their predicates, projections, aggregates, conflict handling, codecs, configurable
limits and framework bindings still require implementation. Native distribution
artifacts must be rebuilt and revalidated from the final source before publication.

### Implemented 19 October 2026: stable live pagination

Server selects now terminate with `paginate({ limit, cursor? })`, returning a typed
`Page<Row>` with public `page` and `cursor` fields. It is mutually exclusive with
`limit` and `unique`. Generated queries returning that direct page expose
`query.paginate(cursor => input)`: an owned live list with readonly flattened data,
`next(pages = 1)`, `hasMore`, subscriptions and disposal. Input remains the authored
function's validated input. Plain lookalike results and output transforms that erase
the page contract do not gain pagination; mutations cannot expose live pages.

The existing PostgreSQL range implementation now uses the shared scalar predicates
and ordering, including multiple sort keys, nullable values and deterministic key
ties. Bounded opaque cursors/ranges bind source, scope, schema and query conditions.
Each refresh reruns authorization and reads its fixed interval in the shared round.
An exhausted final range stays open-ended. Overflow fails explicitly rather than
truncating, refilling or splitting a range. Internal wire metadata carries boundaries
outside user results; no duplicate execution path or compatibility version was added.

Page owners isolate live ranges from ordinary query caches and other lists. Adding
a page captures its boundary before subsequent refreshes. Complete-round staging
includes all derived lists before application callbacks. Append failure retains
successful pages; loaded-page refresh errors apply to the complete list, with the
existing subscriber retention policy. Protected errors win over simultaneous append
errors and clear data even when retention was requested. Auth retirement and disposal
release every page watch and fence an already-resolved first-page await.
Reentrant aborts during initial delivery/watch acquisition release resources exactly
once. Actual callback delivery checks the current auth scope and publication state,
including callbacks queued before a sibling signs out. Input fingerprints and cursor
progress checks reject a mapper or handler that repeats an already-loaded page.

Optimistic query selectors preserve each enumerated cache entry's private identity,
so exact-input and matching targets reach separate page owners with the same input.
Predictions update rows while retaining confirmed continuation cursors. Their target
set remains fixed per invocation, with existing rollback and commit visibility fences.

Current bounds: 1,000 requested/refresh rows per page, 10,000 rows and 16 MiB per list,
128 loaded pages sharing the runtime's 128-query admission limit, and 64 KiB opaque
tokens. Physical-row and wire budgets remain independently enforced. Configuration
of these limits remains an outstanding packet.

Validation passed: four PostgreSQL range cases, six shared-snapshot round cases,
four generated Node/Bun direct-server/service integrations, the native remote-client
malformed-metadata case, 64 browser/HTTP/optimistic cases, database/authoring coverage,
and three focused compiler eligibility/rejection cases. Package typechecking/build,
affected Rust all-target Clippy and formatting, and website documentation checks
(18 cases) and build passed. The compiler tests were split into independent rejection
cases after temporary timing identified highly variable local compiler-process
latency; all assertions were retained and instrumentation removed. The local macOS
native distribution was rebuilt; this is not a new Linux or hosted deployment proof.
The final combined client run passed 62 cases and hit two auth timing failures during
the same local slowdown; those two passed on a focused rerun without source or timing
changes. The final PostgreSQL range rerun also passed after slow compilation.

The [pagination guide](live-pagination.md) and website live-query page now describe
the implemented API and lifecycle. Remaining v1 work includes JSON/array schema and
predicates, projections, aggregates, conflict handling, codecs, configurable limits,
framework bindings, the remaining website rewrite and hosted deployment validation.


### Implemented 19 October 2026: JSON columns and array predicates

The TypeScript schema supports finite `v.number()` columns and explicit JSON storage:
`v.json()`, type-asserted `v.json<T>()`, and structured `v.json(shape)`. Object shorthand
omits only the `v.object` wrapper. Nested object/array shapes are strict, support
optional and nullable members, and reject unsupported values without invoking
accessors or serialization hooks. Native schema descriptors and PostgreSQL JSONB
storage enforce the same shapes; writes replace complete JSON columns. Shapes and
values have bounded depth and validation work. Number function codecs accept finite
JavaScript doubles consistently with number columns; exact int64 retains bigint.

Generated field references and string paths support nested predicates. References
preserve path segments so literal dotted keys cannot collide with nested paths;
strings resolve exact root names before splitting. SQL/path null normalization,
explicit existence checks, exact-element array membership, membership-set helpers,
and callback arrayLength comparisons are implemented. JSON/path ordering is rejected,
including a nested reference whose joined name happens to match a scalar root column.
Unstructured JSON has no runtime string/array shape; ambiguous matching requires a
structured declaration. No implicit nested conversions or partial JSON patches were added.

Validation passed: schema/wire crate suites and doctests, the real PostgreSQL JSON
predicate regression, all four generated Node/Bun direct-server and service integrations,
shared browser/native wire fixtures, focused schema/database/codegen/worker/codec cases,
package typechecking/build, affected Rust Clippy and formatting, Rust authoring consumers,
and CDC diagnostic compilation. The browser service assertions compare JSON content
rather than object prototypes; an initial assertion mismatch was corrected before the
successful rerun. Rust database integration cases ignored by the ordinary facade suite
were not counted as passing. The docs site build passed and local macOS native artifacts
were rebuilt; this does not certify Linux artifacts or hosted deployment.

The JSON guide and website schema page document replacement, null/missing semantics,
array membership and explicit conversions. Remaining v1 work includes projections,
aggregates, conflict handling, broader validators/codecs, configurable limits, framework
bindings, remaining website cleanup and hosted deployment validation.


### Implemented 20 October 2026: field selection and write returning

`select(table, selection)` and write `.returning(selection)` share array and
inclusion-map parsing. Literal inclusions are required result properties; runtime
conditions yield optional properties. False/null/undefined exclusions omit fields,
unknown or empty effective selections reject, and inputs are captured independently
of later mutation. Filters/order use the full row type. Selected row decoding rejects
extra or missing properties without filling defaults. Single/bulk write cardinality,
required where scopes, laziness, memoized execution and mutation rollback are preserved.
Empty bulk inserts now return an empty array without transport, as previously settled.

Native CRUD uses shared projected SQL internals instead of reading full rows and
trimming them in the SDK. Pagination fetches private key/order fields for boundaries
and strips them before delivery. Selected columns and those private fields count
against fetched-row budgets; excluded large fields do not. Projection participates
in cursor/range identity. The existing schema projection contract that requires
entity keys remains separate; ordinary selected results do not require a key.

Validation passed: 14 database runtime tests plus static projection inference checks,
compiler eligibility/conditional-result regression, a real PostgreSQL projection and
pagination budget regression, and four Node/Bun generated direct-server/service cases.
Integration scenarios protect hidden filter/sort fields, projected write cardinality,
constraint rollback, and live refresh shape. Package typecheck/build, affected native
Clippy/format checks, and 18 website docs checks/build passed. The local macOS native
artifacts were rebuilt; Linux artifacts and hosted deployment were not newly validated.
No unrelated database/benchmark suites were run.

The server database guide, pagination notes and website functions page now explain
selection and returning. Remaining v1 work includes aggregates/grouping, conflict
handling, broader validators/codecs, configurable limits, framework bindings,
remaining website cleanup and hosted deployment validation.


### Implemented 20 October 2026: aggregates, grouping and HAVING

The TypeScript database builders now support named count/sum/avg/min/max results,
scalar grouping, callback HAVING filters, group ordering and limits. Ungrouped
aggregates return one object; grouped results return flat arrays with grouping
columns beside aliases. SQL empty/null semantics apply. Invalid projections,
earlier row ordering, duplicate group keys and conflicting aliases reject rather
than silently changing meaning. Grouped pagination remains outside v1.

PostgreSQL performs aggregation in the caller's snapshot and storage scope, with
source-table dependencies retained for live refreshes. HAVING shares the row
predicate compiler through an output-column resolver; generated SQL uses private
aliases so public names cannot alter SQL syntax. Numeric sums/averages compare and
sort numerically before serialization. Int64 sums remain bigint beyond int64 range,
and int64 averages preserve PostgreSQL decimal strings. Generic function bigint
transport now supports these larger results; stored int64 columns retain their
range checks. Non-finite results and unsafe integer counts reject.

Validation passed: 17 focused SDK database/aggregate cases and static inference
checks; PostgreSQL aggregate and existing JSON predicate regressions; shared
native/browser function codec contracts; four generated Node/Bun server and browser
service integrations; package typecheck/build; native Clippy/format checks; and 18
website documentation checks plus its build. The integrations protect precision
across generation and transport, authenticated grouping, live refresh after direct
writes, transaction-local reads and rollback. The SQL test additionally verifies
numeric HAVING field comparisons and scoped empty/grouped results.
Local macOS native package artifacts were rebuilt. Linux artifacts and hosted
deployment were not validated in this slice.

The server database guide and website functions guide now explain the implemented
surface. Remaining v1 work includes conflict handling and expected-row checks,
broader validators/codecs, configurable limits, framework bindings, remaining
website cleanup and hosted deployment validation.


### Implemented 20 October 2026: conflict writes and affected-row expectations

Insert builders now expose one typed `onConflict` policy targeting a real primary
or complete unique key. Atomic update patches use current/incoming expression
references; skips return null for a single insert and are omitted from bulk
results. Returning projections preserve these shapes. Bulk inserts share one
PostgreSQL statement, including its duplicate-target cardinality failure, and
never infer result correspondence from input position. Other database rules remain
enforced. Ordinary update callbacks share the same bounded expression compiler;
compatible whole-column assignments and same-representation numeric addition are
supported, with SQL null propagation and overflow rejection.

`expectRows` accepts an exact nonnegative safe count or a synchronous boolean
predicate across insert/update/delete, including bulk and conflict policies. The
private transport evaluates it before commit and poisons its transaction on
mismatch, exception or invalid result, even if application code catches the error.
Direct root statements, explicit transactions and managed mutations use this same
boundary. A mismatch is distinguishable as `row_expectation_failed`, including
durable browser failure confirmation. Projections do not change the count;
unchanged matched updates count, skips do not. Empty bulk inserts remain inert
unless an attached expectation needs transactional evaluation.

Validation passed: 23 focused SDK database/write/worker cases, static inference
checks, 45 HTTP runtime cases, the real PostgreSQL conflict regression, existing
direct protocol and custom-key-generator regressions, and four generated Node/Bun
direct-server/browser-service integrations. Scenarios cover constraints, scoped
keys, atomic expressions, skip nullability through generation, rollback after a
caught guard failure (including a thrown undefined or an async predicate), and
no-op update counting. Async predicate detection uses the returned value inside
the transaction, so catching a construction-time error cannot bypass that guard.
The initial browser run used an older packaged worker and reported a generic
failure; rebuilding the changed worker and checking the remote error contract
resolved it, and both browser runtimes passed. Package typecheck/build, affected
native Clippy/format checks, and 18 website checks plus its build passed.
Local macOS native package artifacts were rebuilt; hosted deployment and Linux
artifacts were not newly validated.

The server database and website functions guides document the new surface.
Remaining v1 work includes broader validators/codecs, configurable limits,
framework bindings, remaining website cleanup and hosted deployment validation.


### Implemented 20 October 2026: field codecs, temporal storage and execution limits

TypeScript fields now support deterministic synchronous application/storage
codecs. Nullable values and omitted defaults bypass callbacks. Whole-value
comparisons encode application values, while patterns, arithmetic and custom-codec
aggregate results retain SQL storage semantics. JSON member conversions require
explicit codecs. Generated types retain storage capabilities without treating
custom classes as nested JSON fields. Default int64 representation is a safe
JavaScript number; explicit bigint mode preserves the full stored range and wide
aggregate sums.

Date and timestamp declarations now map to actual PostgreSQL temporal columns,
with calendar strings, UTC Date values, zone-free clock strings and explicit
precision choices. Precision loss, invalid calendars, unsupported Date precision
and infinite timestamps reject. Driver binary I/O avoids DateStyle and session
timezone ambiguity. Built-in Dates survive generated browser inputs/results as
separate Date instances; arbitrary custom classes require an explicit portable
function output. Native temporal predicates, defaults, grouping, extrema and
cursor ordering use the same storage contract.

`defineConfig({ limits })` now supplies execution, input/output, heap/stack,
operation and fetched-data defaults, with partial overrides on final query/mutation
declarations. Extensions cannot alter budgets. Input checks precede validation;
output checks follow transformations and precede commit. Generated direct database
handles inherit the configuration. PostgreSQL row streams charge binary column
payloads before collection, including write returning and private cursor/version
columns. A caught budget failure still fences commit. These read budgets bound
collected data rather than SQL scans or hard native process RSS.

Native/HTTP frames and generated browser response limits follow effective payload
budgets. Public browser configuration now accepts request, overall one-shot query,
confirmation and stream-idle deadlines. Queries retry transport interruptions
within an overall deadline; server errors terminate. Mutation confirmation still
checks the original submission without replay. Development warnings report
measured budget usage without SQL or submitted values.

Validation passed: focused SDK codec/schema/database/write/aggregate tests and
static inference, shared native/browser payload contracts, PostgreSQL temporal and
streaming-read-budget tests, existing conflict/aggregate regressions, and all four
generated Node/Bun direct-server and browser-service integrations. The generated
integrations cover Date transport, custom-codec defaults and predicates, unsafe
integer rejection, payload overrides above 16 MiB, pre-validation rejection, and
rollback after output/read/operation/execution budget failures. The broader SDK
run initially reported four failures: default-omission and optimistic-cache fixture
expectations were corrected and their targeted tests passed; the transitive-import
test passed on an isolated rerun; the dev lifecycle test passed outside the sandbox,
whose macOS filesystem watch events were blocked. No dev watcher change was needed.

Package typecheck/build and native artifact verification passed. Native workspace
all-target/all-feature compilation was used because Scalar/Value/FieldDefinition
changed across consumers; affected Clippy/format checks passed. The macOS native
package artifacts were rebuilt. Eighteen website documentation checks and the site
build passed. Linux artifacts and hosted deployment were not newly validated.

The schema/function/runtime website guides and the new field-codec/execution-limit
guides document the implemented behavior. Framework bindings remain a separate
planning and implementation stage, as requested. Remaining v1 work also includes
broader constraint/validator coverage, remaining documentation cleanup and hosted
deployment validation.


### Implemented 20 October 2026: remaining core schema API

Scalar columns now support inclusive numeric bounds, Unicode-codepoint string
bounds and literal-inferred `oneOf` choices. Rules validate encoded values and
defaults and become PostgreSQL checks, including for expression updates.
Comparison and arithmetic operands validate their storage representation without
being restricted to possible stored values. Explicit immutable `.index(...)`
declarations and named `sql` template checks travel through both descriptors and
Rust schema generation. Interpolated root fields and scalar values remain
separate from trusted SQL expression text; table checks follow PostgreSQL null
semantics and do not become standalone input validators.

`v.id(name)` and `v.id(table)` resolve registered primary keys, representations and
deletion policies. The superseded `.references` authoring path is removed.
Restrict remains the default; cascade and nullable set-null writes share rollback,
scope isolation, accounting and revision/invalidation. Set-null updates advance
concurrency versions without double-counting managed updates. Acyclic primary-key
references support one-to-one extensions without unrelated key generation;
unresolvable primary-key cycles reject. Generated schema aliases record resolved
table identities so structurally identical table declarations cannot silently
weaken reference types. ID types accept validated scalar inputs while rejecting
known IDs from another table. Incompatible reuse of a table in another schema
rejects instead of changing an adapter's key validator silently.

Tables expose `$inferSelect`, `$inferInsert` and `$inferUpdate`. Optional
`@hulla/db/zod` exports native strict select/insert/update schemas, with refinement
callbacks, codec-aware validation and omission policies. Default values are not
inserted by validators and no implicit coercion occurs. Literal choices survive
native refinements/transforms and generated declaration inputs. Zod stays an
optional peer, with no new resolved dependency versions or core runtime import.

V1 bounds/choices apply to scalar columns. Constrained JSON members reject
explicitly because the current native JSON shape contract cannot preserve those
rules. Use function validators or named SQL checks instead. Reference-specific
field constraints/codecs also reject consistently; target primary keys own their
domain, and subset rules can use SQL checks.

Validation passed: all 192 SDK tests and the package typecheck/build; native schema
and executable documentation tests; PostgreSQL storage enforcement, index/check
DDL, all deletion policies, expression rollback, scope/version/revision behavior
and descriptor reconnect regressions; all four generated Node/Bun direct-server
and browser-service integrations. The browser fixture additionally compiles its
actual generated Zod-input-to-database consumer and observes child-only views
refreshing after parent cascade/set-null writes. The shared authored Rust fixture
compiles emitted constraint/index/check/reference metadata and validates writes.
Affected native Clippy/format checks, native artifact verification, 18 website
checks and the site build passed. macOS native package artifacts were rebuilt;
Linux and hosted deployment were not newly validated.

Integration tests caught an unavailable Web encoder during restricted schema
evaluation and literal-union widening in Zod output types; both are fixed. The
full SDK run also exposed Bun rejection matchers starving compiler RPC I/O:
awaiting compiler outcomes before synchronous error assertions reduced one case
from 43 seconds to 123 ms without changing production transport. Temporary traces,
debug output and abandoned fixtures were removed.

The schema guide and website schema/reference pages now describe the completed
API and remove obsolete SDK exports, row wrappers and persisted-browser recovery
instructions. The next stage is framework bindings and authentication reference
integration, followed by the final publication/export audit and portable hosted
deployment validation.

### Implemented 20 October 2026: framework query/mutation bindings and SSR handoff

Separate `@hulla/db-react`, `@hulla/db-vue`, `@hulla/db-svelte` and
`@hulla/db-solid` packages share one framework-neutral observer implementation
and the existing generated browser runtime. Framework/core packages are peers;
the core gains no UI framework dependency. A Bun workspace supplies one lockfile
and a small shared binding build command; compiler build invocations are bounded.

Core `Input`/`Output` replace the superseded inference helper names and support
authored declarations, both generated callable kinds and optimistic wrappers.
Queries infer readonly resolved data; mutation output retains its declared shape.
No-input inference does not mistake query call options for submitted input.

The UI packages provide function-first query observation, reactive inputs and
callable mutation wrappers with framework-native `.state`. Observers start after
mount and stop with the component; rendering performs no network work. Mutation
state starts idle and follows the latest-started call while each call retains its
own awaitable outcome. No mutation is cancelled or resubmitted by UI cleanup.

Optional `initialData` is an observer-local, stale pending SSR fallback; it never
enters the shared authoritative or optimistic cache. Protected seeds require an
app-provided `initialAuthKey` matching browser auth metadata. Input or identity
changes permanently retire the fallback, including switching back to an earlier
input/account. Explicit app auth updates automatically reattach subscriptions;
promise loading pauses them. A server rejection stays an error without an auth
retry loop. Direct server database access remains application-owned.

Tests protect generated type inference, SSR request isolation, real React
hydration, StrictMode cleanup, equivalent inputs, obsolete callbacks, out-of-order
mutations, actual Vue/Solid lifecycles and compiled Svelte components. Real HTTP
runtime tests cover scope changes, auth promises, expiry and server rejection.
Follow-up work includes paginated UI helpers, Better Auth storage/schema support,
complete meta-framework recipes, the publication/export audit and hosted proof.


### Implemented 20 October 2026: paginated UI bindings and Better Auth storage

All four UI packages now expose function-first paginated observation with a cursor
mapper, flattened readonly data, `hasMore` and stable `next(pages = 1)` delegates.
They share the core page handle, retain pending rows while appending, reset all
ranges on changed first-page input, retire identity/SSR fallbacks and release every
page on unmount. Internal observer state joins coordinated notification batches
before application subscribers run; the core keeps one cache and paging engine.

The separate `@hulla/db-better-auth` package implements the Better Auth 1.7.12
adapter contract against direct PostgreSQL access. It supports model/column
mapping, filters, selections, counts, provider offset reads, transactions and
single-row credential consumption. Application/provider transaction callbacks
run once; only internal database-only work retries confirmed OCC rollbacks.
The provider's portable join fallback stays in use. Authentication endpoints,
passwords, cookies and authorization stay with Better Auth and the application.

The schema subpath supplies default/core tables, configured/plugin schema mapping
and a static generator. Auth IDs remain opaque strings, field types and nullable
semantics are validated, indexes/unique keys and foreign keys remain database
constraints, and unsupported policies fail explicitly. The schema helper uses
the existing Better Auth core dependency's portable entrypoint. Static schemas
avoid importing server plugin constructors into restricted query workers.

Full-setup validation exercises generation before the auth database exists and
then regenerated identity/date declarations and their inferred types. Generated
server `db` now has an explicit schema-bound type, breaking the inference cycle
between config auth, Better Auth and that database. Worker bundling checks emitted
external imports instead of mistaking Bun's pruned input imports for dependencies;
its host/dynamic-import guards remain enforced.

Relevant runtime tests, bounded package typechecks/builds, local PostgreSQL signup,
signin, durable sessions, signout, transaction rollback, credential/counter races
and FK cascades passed under Node and Bun. The Makefile's `better-auth-test` runs
both PostgreSQL fixtures and owns their namespace cleanup. Repo and website guides
cover bootstrap, auth verification/cache changes, pagination and SSR. The website
integrity tests and production build passed. The development watcher requires
native filesystem event access and passed outside the sandbox after its sandboxed
timeout. No hosted deployment was exercised.

Remaining integration work is complete meta-framework recipes and deployment
fixtures, followed by the publication/export audit and portable hosted proof when
non-production targets are available.


### Implemented 21 October 2026: meta-framework recipes

A shared task setup and focused recipes now cover Next App Router, Nuxt 4,
SvelteKit/Svelte 5, SolidStart 2 and Astro React islands. Each shows its actual
browser startup boundary, a direct server read, typed SSR initial data, live query
observation and a browser mutation. Shared guidance covers provider verification,
account-key handoff, private caching, shared database helpers, pagination and
native deployment requirements. No new framework-specific SDK package, runtime
factory or production dependency was introduced.

The examples were extracted into disposable projects and passed real database
binding generation plus TypeScript checks of shared helpers/declarations and
live component scripts. Vue SFC/script/templates, Svelte server/client components
and Astro page syntax were compiled with available framework compilers; React
and Solid JSX/config snippets were parsed as well. This is example validation,
not a new full application-build or hosted-deployment proof for every framework.
The generated projects and compiler processes were cleaned up.

Website guides/navigation now expose all five recipes and the shared setup.
All 18 documentation integrity tests and the production website build passed.
The existing route-integrity check now discovers nested package documents rather
than overlooking framework subpages. Superseded follow-up wording in the binding
guide was removed. Remaining work is the publication/export audit, additional
framework deployment fixtures and portable hosted proof when targets exist.

### Implemented 21 October 2026: website documentation rewrite

The website database docs now start with the TypeScript application surface and a
working React/PostgreSQL quickstart. Navigation separates getting started, database
operations, application integrations and deployment. Focused guides cover direct
builders, constraints, codecs, identity and hosting. Native Rust authoring has a
separate guide. The obsolete session/predictor optimistic-write guide was replaced
with the actual generated callable and `.withOptimistic` API.

The quickstart's actual code blocks generated and typechecked in a disposable
application, initialized an isolated PostgreSQL namespace, saved tasks through the
browser, updated a second live tab and retained rows after reload. Follow-on
function, database, optimistic-wrapper and codec examples typechecked against the
generated SDK. Eighteen website integrity tests, database-page formatting, the
production build and a built-page link/anchor audit passed. The introduction and
quickstart were inspected in the desktop browser. Temporary application files,
processes and the owned database namespace were removed after validation.

The reference describes the current sanitized remote-error contract accurately.
Intentional custom DatabaseError messages/details are not preserved by the worker
and HTTP protocol today; their browser propagation needs a dedicated protocol and
security review before publication. This rewrite does not claim that gap is fixed,
or replace the remaining hosted-deployment and publication/export checks.

### Implemented 21 October 2026: public SDK export cleanup

The package root now exposes application errors, callable/handle/state types,
auth metadata and Input/Output inference helpers. Generated browser bindings use
an internal client entrypoint for transport setup, codecs and binding factories.
Generated declaration constructors and workers use separate internal entrypoints;
these retain their browser/worker execution boundaries. The redundant
schema-taking declaration factory was removed, and generated constructors no
longer import the authored schema at runtime solely to pass an unused argument.

The superseded Node/Bun request adapter was deleted with its two dedicated tests:
they protected remotely invoking declarations in server request scopes, a behavior
removed in favor of direct generated server db access and shared helpers. They do
not replace the direct database request/transaction isolation coverage. Public
query snapshots now expose the agreed state and stale/optimistic indicators,
without connection bookkeeping or pending prediction IDs. Svelte no longer
forwards those implementation fields. The package README and website reference
no longer teach manual HTTP runtime construction or public worker/declaration
bootstrap imports. No compatibility aliases or archived execution paths remain.

Validation passed: the 124 affected core cases across the focused run and repaired
codegen/optimistic rerun; all 25 framework/core-binding cases; bounded sequential
TypeScript checks for the core, React, Vue, Svelte, Solid and Better Auth; core and
Svelte builds; generated PostgreSQL service integrations under Node and Bun; and
installed-tarball native startup plus bundling actual generated browser callables.
The initial failures were old optimistic-view expectations and a bundle test that
resolved stale built exports instead of inheriting the source condition. Updated
expectations and the existing runtime-condition helper resolved those failures.
All 18 website integrity checks and the production site build passed. Task-owned
integration namespaces and services were cleaned up. No Rust behavior changed,
and hosted deployment was not rerun.

The agreed database.server/database.client config reshaping and automatic browser
setup remain the next implementation step; this cleanup deliberately preserves
the existing generated configure call until that replacement is implemented.
Root authoring-import consolidation and optional optimistic input mapping remain
separate syntax decisions rather than additional aliases in this cleanup.

### Implemented 21 October 2026: optimistic target shorthand

No-input query targets now accept just query and update. Input-bearing targets
require an exact input mapper or a matching predicate; matching no longer requires
an unused mapper. Query-specific input/result inference and readonly cache data
are preserved across multiple heterogeneous targets. Runtime validation still
rejects an input-bearing target without either selector. Explicit undefined input
contracts also permit the shorthand rather than disagreeing with their callable
types. Schema and config imports remain separate, as requested.

Existing runtime tests now exercise the shorthand through actual generated
callables and the HTTP cache, match-only filtered targets, exact filtered targets,
page ownership and rollback/confirmation behavior. Compile-time regressions reject
missing selectors and invalid match/mapping types while retaining per-target
inference. All 80 affected cases and core typechecking passed. The core build,
built-module targeting checks under Node and Bun, all 18 website integrity checks
and the production documentation build passed. The README, architecture and
optimistic-update guide use the new syntax. No database or native worker behavior
changed, so PostgreSQL/hosted deployment suites were not rerun for this change.

### Implemented 21 October 2026: full website API audit

All 24 database documentation pages were checked against the current SDK;
18 needed updates. The audit corrected mismatched query names and input shapes,
aligned framework and Better Auth recipes with the quickstart table, fixed a
generated import path, explained same-origin routing and separate credential
transport, and removed optional hot-reload bookkeeping from primary setup steps.
The guides now cover current JSON path/array predicates, null and presence checks,
write expressions, conflict policies, grouped aggregates, extension input conflicts
and the public/generated import boundaries.

Extracted examples passed 64 TypeScript checks against actual generated SDK
bindings, including authenticated declarations and all five meta-framework live
components. Vue and Svelte components also compiled. All 18 website integrity
checks, database-page formatting, the production build and 4,538 built database
links/fragments passed. These are documentation/example checks, not full builds
or deployment proofs of every framework application. Scratch projects and owned
compiler/build processes were cleaned up; no database runtime changed.

Two previously agreed conveniences remain implementation gaps: config-driven
browser setup using database.server/database.client, and bare v.object/v.array
table fields as JSON-column shorthand. The website documents the executable
configure call and explicit v.json wrapper until those replacements exist. The
earlier remote DatabaseError message/details gap also remains outstanding.

### Implemented 22 October 2026: automatic browser setup, JSON shorthand and declared errors

`database.server` now owns the private PostgreSQL URL/namespace and
`database.client` contains explicitly validated public HTTP settings. Generation
copies only those public settings into the client runtime. Browser imports stay
lazy: the first call creates the transport, while `setAuth` and `setHeaders` update
identity and request credentials separately. Root-relative service URLs resolve
against the browser origin. The superseded generated `configure` path and its
manual-configuration ownership test were removed; the retained browser/config
cases protect lazy startup, public-only generation, identity changes and disposal.

Bare `v.object` and `v.array` table fields normalize to JSON columns while retaining
inferred values and nullability. Whole-column optional values are rejected;
optional members inside JSON objects remain supported. Existing schema, generated
inference and real-service scenarios exercise these cases.

A deliberately thrown `DatabaseError` now preserves its code, message and JSON
details across the restricted worker, rollback/recovery storage, HTTP/SSE and both
TypeScript and native clients. The public payload is bounded to 16 KiB, excludes
stack/cause and cannot reinterpret an application code as a transport/authentication
control signal. Ordinary exceptions remain sanitized. Recovery quota reserves now
cover the bounded failure payload after a rolled-back mutation. The service tests
exercise reserved-looking application codes, explicit null details, discarded
writes, receipt recovery and oversized errors.

Validation passed all 195 SDK cases, Node and Bun service integrations, focused
worker-failure/query-round/quota tests, native HTTP/error cases, affected Rust
formatting/lint checks and installed-package/browser bundling checks. The matching
website update passed 18 integrity tests and the production build. These results
close the three SDK gaps listed in the preceding audit.

The hosted proof then found that Neon's default URL requires SCRAM channel
binding. The custom TLS adapter was replaced by the maintained
`tokio-postgres-rustls` adapter. Verified certificates/hostnames remain required;
there is no authentication downgrade. The new ignored regression passed against
disposable Neon with `channel_binding=require`; untrusted-certificate rejection
also passed. Both macOS arm64 and Linux x64 binaries were rebuilt. A scoped OSV
query found no advisories for the added adapter/parser dependencies.

The rebuilt package passed actual Vercel-to-Neon route writes, SSR, transaction
rollback and anonymous rejection. The full installed-package local browser proof
also passed against Neon, including live updates and active-session revocation.
The hosted browser proof now also passes through Vercel's same-origin rewrite to
a dedicated Render Linux service and Neon. A real Render restart replaced the
service instance; the browser reconnected without reloading and received another
committed write. Hosted server-action writes, grouped rollback, anonymous rejection
and private-credential exclusion passed too. The exact Linux image separately
passed local authentication and a restricted query against Neon. See
[deployment evidence](../deploy/cloud/proofs/2026-10-22.json).

This completes the requested implementation and hosted proof-of-concept work.
Publication metadata and final release validation are separate work. Sustained
load, transaction poolers and provider suspension/wake behavior were not tested.


### Prepared 22 October 2026: npm and Cargo release candidates

MIT was approved for the first `0.1.0` release. The new private `hulladev/db`
repository is configured as origin; no source push or registry publication was
performed. Six npm archives and nine production Cargo archives now have release
metadata and license text. Internal Cargo dependencies specify registry versions;
the diagnostic CDC crate remains unpublished.

The npm release builder stages clean production manifests, strips checkout-only
source conditions/tooling, includes the verified macOS arm64 and Linux x64 native
assets, and preserves native third-party license/copyright notices. Generated
artifacts and their SHA-256 inventory live under `output/releases/0.1.0/`. Source
workspace packages remain private so publication uses the reviewed tarballs.

Cargo packaged and compiled all nine archives with all features. A second check
extracted those archives outside the checkout and compiled every target/feature
against the extracted siblings. Checkout-only npm/PostgreSQL integration tests
are excluded from Cargo publication; private unit-test modules remain present.
The six npm archives passed external installation, actual application generation,
framework/Better Auth declaration checks, browser bundling, license-file checks,
packaged native startup and npm publication dry runs. Release tooling passed strict
TypeScript checking. These are archive checks; PostgreSQL/hosted behavior results
from the preceding implementation remain applicable because runtime code did not
change during packaging.

The release guide records commands, publish order and target limits. The candidate
manifest marks the current dirty checkout; review/commit the complete source and
regenerate from that commit before an actual release. Public repository visibility,
registry publishing credentials and publication remain separate explicit steps.
