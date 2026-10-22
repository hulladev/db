# Production operations and self-hosting

The registered-function runtime now has separate operator/runtime/backup/worker database
roles, durable scope quotas, per-principal host admission budgets, typed native
admin transactions, Node/Bun request integration and a self-hosted container
starter. These are deployment primitives and an application example, not a
managed cloud control plane. Publication compatibility and upgrade migrations
remain deliberately deferred.

## Database roles and transport

An operator connection initializes the canonical schema, explicitly creates data
scopes, sets quotas and calls `ManagedStore::provision_roles(&Roles)`. Provisioning
creates four dedicated unprivileged `NOLOGIN` groups and refuses existing groups
with login/admin attributes or inherited memberships. The owner group owns the
namespace and tables. The runtime group receives schema usage, reads, application
row DML, scope sequence updates and receipt insertion. It cannot create scopes,
change quotas/source, truncate tables or install DDL. Bounded receipt cleanup has
DELETE and restricted UPDATE grants on receipt metadata; admission and ticket
maintenance also use their dedicated tables. The backup
group receives reads only. The worker group reads metadata and outbox jobs and
updates delivery bookkeeping; it cannot access application tables or change job
payloads/source fences. Runtime connections insert jobs but cannot claim or
acknowledge them. See the [transactional outbox](outbox.md) for worker ownership
and operator retry/discard APIs. Table and column grants to PUBLIC are revoked.

`grant_role(group, login)` binds an existing dedicated unprivileged login and
refuses administrative attributes, ownership and unrelated inherited groups. The
operator creates passwords separately; provisioning never prints or changes them.
Use separate groups and credentials for independent deployments. These grants
protect against accidental administrative writes. A compromised serving database
credential can still execute application-row SQL; only the trusted service holds
it, and SQL writes outside managed transactions are unsupported. Browser clients
never receive PostgreSQL credentials. Database-level CONNECT/TEMP privileges and
other application namespaces are operator responsibilities.

Managed connections use verified TLS with `sslmode=require`, checking certificate
chains against WebPKI roots and the configured hostname/IP. The maintained
PostgreSQL rustls adapter supplies SCRAM channel binding, including connections
with `channel_binding=require`. Private CAs and client certificates are not
configured by this connector.
Implicit plaintext preference is accepted only for localhost/loopback/Unix socket
connections. Remote plaintext requires explicit `sslmode=disable`, used by the
starter on its isolated container network. TCP connection attempts have five-second
limits; TLS/authentication/session setup share a ten-second deadline. Statement
and lock waits have five-second limits. Configure an encrypted network or a
publicly trusted PostgreSQL certificate when that isolation is insufficient.

## Storage and admission quotas

`set_scope_quota(scope, ScopeQuota { .. })` persists optional nonnegative limits;
`None` disables a limit and zero denies consumption. Lowering a limit below
current usage fails. Every managed mutation checks the scope's total row count,
stored data bytes, stored receipt bytes, serialized mutation-result bytes and
optional retained outbox job/payload limits inside the transaction, after writing
its receipt and before COMMIT. Exceeding a
limit rolls back rows, versions, revision and receipt together. Existing request
recovery remains possible at capacity. `scope_usage` reads a consistent snapshot.
These checks apply to admin transactions too and coordinate through PostgreSQL
across service instances. Usage scans configured scopes on every write; measure
latency and serialization retries with representative tenant sizes before scaling.

Stored bytes use PostgreSQL `pg_column_size` of tuples, excluding indexes, WAL,
page overhead, backups and some external TOAST storage. This is an accounting
budget, not a filesystem ceiling. Result limits here apply to mutation results;
query/SSE response and request limits remain host transport budgets. Ticketed browser mutations retain results for bounded confirmation, then compact
and purge admission records after retiring their ticket. ACK can release a delivered
result early. The trusted unticketed native API retains permanent request markers;
operator compaction removes results without permitting execution again. Inputs are
bound by a fingerprint, not retained as a second JSON copy. See [operational visibility
and receipt retention](operations-visibility.md) for the distinct retention policies,
host statistics and reconciliation contract. These records count against quota. Each pending admission reserves 16 KiB plus
128 bytes for publishing a bounded intentional failure after rollback.

`managed::Options::principal_quotas` configures requests per fixed 60-second window,
in-flight requests, live subscriptions and bounded tracked identities. Counters
are keyed by verified scope/principal; saturation returns HTTP 429 `capacity`.
Stream permits live until disconnect/disposal. These admission budgets are
per-process and reset on restart; multiple hosts need an upstream shared limiter
for deployment-wide request limits. Global connection/request/subscription
budgets still apply. Authentication, ingress bytes and unauthenticated traffic
also need the proxy's admission policy.

## Framework requests and native administration

Node/Bun application code imports `db` from its generated server entrypoint. The
framework or application authenticates each request, then uses ordinary database
calls. Share ordinary helpers with query/mutation handlers when needed. Direct access
does not execute declarations, create browser recovery receipts or infer a user.

```ts
import { db } from './db/_generated/server'

// In an application route, after verifying the session:
const tasks = await db.select('tasks').where({ owner: session.user.id })
```

`session` comes from the application's trusted auth verifier. Never accept an owner
or identity from a request body as proof. Use `db.transaction(async tx => ...)` for
writes that must roll back together. Share the generated handle across requests;
`db.close()` belongs at application shutdown, not at the end of each request.
See [direct server access](server-database.md) and [portable deployment](deployment.md)
for lifecycle bounds, native assets and the separate persistent realtime service.

`hulladb::AdminDatabase::connect(url, schema, trusted_context)` requires operator
schema CREATE privilege and pins the current deployment source. Its typed `query`
and `transaction` callbacks receive the same typed database contexts as registered
functions. Admin callbacks intentionally bypass business authorization; codecs,
constraints, row versions, quotas, revisions and receipts still apply. Keep this
object and its credential outside serving routes and browser imports.

```rust,ignore
let mut admin = hulladb::AdminDatabase::connect(&operator_url, schema, job_context).await?;
let committed = admin.transaction("jobs.seed", stable_request_uuid, input, |ctx, input| {
    Box::pin(async move {
        let first = ctx.db.insert(input.first).await?;
        let second = ctx.db.insert(input.second).await?;
        Ok(vec![first, second])
    })
}).await?;
```

Use a stable operation name, explicit typed input and persisted job request UUID.
Retrying that identity recovers the receipt without executing the callback again.
Captured state must not change the meaning of the input. Admin callbacks use scope
coordination and built-in/default keys; custom registry generators are not currently
injected into this convenience API. Admin is an explicit trust boundary, not an
implicit elevation of framework calls.

## Self-hosted starter

From the repository root, install Docker Compose v2 and Python 3.9+, then:

```sh
python3 deploy/selfhost/setup.py
mkdir -p deploy/selfhost/backups
export HULLADB_OPERATOR_UID=$(id -u) HULLADB_OPERATOR_GID=$(id -g)
docker compose -f deploy/selfhost/compose.yaml build app backup
docker compose -f deploy/selfhost/compose.yaml up --detach --wait --wait-timeout 60
curl --fail http://127.0.0.1:8080/health/ready
```

Setup refuses an existing secrets directory. Its parent is mode 0700; individual
files are readable inside non-root Compose containers. Store and inspect these
files privately; never commit them, print them into logs or send them to clients.
`credentials.json` contains an operator-created bearer and its trusted identity
mapping. Replace the sample `selfhost/application.rs` with your schema, registered
functions and policies, and replace `FileAuth` with your login provider if needed.
The starter registers `entries.list` and `entries.create`; owner policy filters
reads and checks stored receipt ownership on recovery.

PostgreSQL 18.6 has a persistent volume and no published port. On a fresh volume,
its init script creates runtime and backup logins with separate generated passwords.
The one-shot initializer alone mounts the operator URL and installs storage,
scopes, quotas and grants. The serving container mounts only the runtime URL and
credential mapping; it verifies rather than installs storage. App containers run
as UID 10001 with read-only roots, dropped capabilities and CPU/memory budgets.
`/health/live` reports the process; `/health/ready` verifies database access/layout.
SIGTERM stops the host, retires streams and allows graceful shutdown.

Default per-scope budgets are 100,000 rows, 128 MiB each of tuple data and receipts,
32 KiB mutation results, 10,000 outbox jobs and 16 MiB of outbox payload/name data.
Compose supports `HULLADB_MAX_ROWS`,
`HULLADB_MAX_DATA_BYTES`, `HULLADB_MAX_RECEIPT_BYTES`, `HULLADB_MAX_RESULT_BYTES`,
`HULLADB_CORS_ORIGINS` and external `HULLADB_PORT`. See the example's environment
settings for connection and principal admission budgets when embedding it.
Run initialization explicitly after changing quotas or adding scopes. Do not
regenerate secret passwords against an existing volume: PostgreSQL's first-boot
script does not update existing roles. Rotate database passwords and corresponding
URL files together under operator control.

The app port binds loopback. Place a TLS reverse proxy in front of it for remote
clients, explicitly allow browser origins, preserve streaming SSE responses,
disable proxy buffering and choose timeouts beyond credential/heartbeat intervals.
The credential file is reread on authentication and stream credential checks;
atomically replace its contents to revoke/rotate application tokens. Change
`authorization_scope` whenever identity/permissions change, so existing streams
retire their old verified context. Newly added
scopes require initializer provisioning. Protect public readiness probes at ingress
if the deployment should not expose database availability.

## Backup and restore drill

`scripts/backup.py` uses the installed `pg_dump`, `pg_restore` and `psql` via libpq
PG* environment or `HULLADB_PG_PASSWORD_FILE`. Use the same PostgreSQL major version
as the server. It backs up one dedicated namespace in a consistent custom archive,
extracts source from that archive snapshot and publishes mode-0600 archive/manifest
files without overwriting existing paths. The manifest records source, namespace,
UTC time and SHA-256. Roles, passwords and ACLs are excluded. Archives are trusted
operator input: PostgreSQL restore can execute SQL they contain. Encrypt and copy
both files off-host; a checksum alone does not authenticate an archive.

```sh
# HULLADB_OPERATOR_UID/GID are set as above. Use a fresh output filename.
docker compose -f deploy/selfhost/compose.yaml run --rm backup \
  backup --schema hulladb_app --output /backups/drill.dump
# Create an isolated drill database; this does not touch the serving database.
docker compose -f deploy/selfhost/compose.yaml exec -T postgres \
  createdb --username=postgres hulladb_restore_drill
docker compose -f deploy/selfhost/compose.yaml run --rm \
  -e PGDATABASE=hulladb_restore_drill restore \
  restore --archive /backups/drill.dump
```

Restore verifies checksum and refuses an existing target namespace. It restores
and rotates source within one transaction; any SQL/source validation failure leaves
no partial namespace. Rows, versions, revisions, quotas and original receipts are
preserved, but the deployment gets a fresh source UUID. Outbox jobs retain their
IDs, payloads and old source fences and require explicit operator reconciliation
before current-source workers can deliver them. Old receipts retain their
old source and cannot be silently rebound to a new lifecycle. Clients with stale
sources must retire subscriptions/cache coverage and reconcile unresolved work
explicitly; writes after the backup may be lost. Revision monotonicity alone cannot
identify a restored deployment.

For the drill, inspect row counts, quota rows, request receipts and the new source
using the operator connection. Reapply role provisioning against the restored
database, start a separate serving process with a runtime URL pointing there, then
verify authorized queries, a new mutation and a fresh subscription. Record archive
age (RPO) and elapsed restore-to-ready time (RTO), along with row/receipt comparison.
Only remove the explicitly created drill database after that evidence is saved.
Do not promote it by editing the live database in place.

For a real recovery, stop serving, preserve the damaged database for investigation,
restore into a separate database, reapply grants, verify integrity and application
behavior, switch the runtime credential URL and restart serving. Keep the previous
database until rollback policy permits its removal. Provision logins separately
when recovering onto a new PostgreSQL cluster. A namespace dump does not provide
whole-cluster point-in-time recovery, external assets, role backup or WAL retention.
Restoring renders SQL into private temporary storage; increase the operations
container's 256 MiB `/tmp` budget for larger archives.

## Validation and remaining deployment work

`make production-check` selects the affected Rust crates, quota unit tests and SDK
checks. `make production-test` runs PostgreSQL quota/privilege/source/admin cases;
set `HULLADB_TEST_URL` for a nondefault test database. Export
`HULLADB_OPERATIONS_TEST_URL=postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev`
for the development backup drill, or another explicitly chosen privileged URL.
The drill creates/removes only its own unique fixture namespace and restore database. Broader managed runtime and HTTP
integration checks remain `make db-test sdk-adapters-test`.

Before operating a real deployment, choose the TLS proxy and authentication
provider, off-host encrypted backup schedule, monitoring and alert thresholds,
receipt retention policy, capacity envelope and incident ownership. Exercise the
drill with representative data and record RPO/RTO. The starter is single-region;
shared admission limiting, failover/PITR and managed-cloud provisioning are separate
operator/platform integrations. Schema/protocol/file-format compatibility and
migrations are added at publication, never by silent fixture reset.
