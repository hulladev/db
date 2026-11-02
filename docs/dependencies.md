# Dependency boundaries

The SDK has no required npm runtime dependencies. Browser bindings and production
Node/Bun entrypoints use platform APIs. Generation and `dev` use a privately packaged
Bun runtime and TypeScript 7.0.2 compiler, rather than an application peer dependency.
Development assets include a local PostgreSQL 18.6 build; an explicit connection
does not start or initialize it. Production entrypoints do not load this tooling.
Zod 4 is an optional peer for `@hulla/db/zod`: applications
choosing that adapter supply it; ordinary core entrypoints do not import it.
Valibot and `@types/*` remain development fixtures/tooling. Prepared `serve` does not import the
compiler. Native executables are built before packaging, without install scripts
or installation-time downloads.

Local PostgreSQL is built from the official 18.6 source archive with its pinned
SHA-256. The build disables optional ICU, readline, compression and TLS libraries;
it listens on loopback with PostgreSQL's SCRAM authentication. Original PostgreSQL,
TypeScript and bundled Bun notices accompany the assets. The October 26 check of
[PostgreSQL 18 advisories](https://www.postgresql.org/support/security/18/) found no
listed fix newer than 18.6. This is scoped to the added PostgreSQL dependency.
The distributed Bun runtime is pinned to 1.4.4, which includes the upstream
[October 22 security fixes](https://bun.sh/blog/bun-v1.4.4). This avoids shipping
the older maintainer runtime as a newly introduced application dependency.

Separate UI packages use their matching framework and `@hulla/db` as peers.
`@hulla/db-better-auth` uses Better Auth as a peer and its already-resolved
`@better-auth/core` 1.7.12 package directly for portable schema discovery. The
core database gains no framework or authentication dependency. The October 20
integration update passed `bun audit` against 101 resolved packages; this is an
npm advisory check, not a new Rust workspace audit.

## Rust components

| Component | Necessary dependencies | Excluded from its production graph |
| --- | --- | --- |
| HTTP service / direct bridge | Tokio, Axum/Hyper, PostgreSQL driver, Serde, UUID, rustls/ring and roots | SQLite, CDC/replication, certificate test generator |
| Restricted handler process | QuickJS via rquickjs, Serde JSON | Tokio, HTTP, PostgreSQL, SQLite |
| Native remote SDK | HTTP/TLS transport and SQLite for durable mutation recovery | PostgreSQL when built with `--no-default-features --features remote` |
| Rust authoring macros | syn, quote, proc-macro2 at compilation | Proc-macro implementation does not become an application runtime service |
| CDC diagnostic crate | Replication protocol, SQLite event log and its capture utilities | Not a dependency of the npm binaries or generic SDK |

The workspace lockfile includes development dependencies, optional features and
other target platforms. Its package count is not a count of libraries bundled in
each executable. Inspect a production root with normal/build edges, then inspect
individual features when a surprising dependency appears:

```sh
cargo tree -p hulladb_server --features typescript --edges normal,build
cargo tree -p hulladb_js --edges normal,build
cargo tree -p hulladb --no-default-features --features remote --edges normal,build
```

The October 19, 2026 host audit resolved 106, 16 and 63 unique package/version
entries respectively, including local crates and build dependencies. These counts
are observations, not thresholds or regression tests. The compatibility HTTP
transport crate has no normal dependencies with its `http` feature disabled.
In 0.1.1, enabling it forwards to `hulladb/http`; new consumers use that SDK feature
directly. `hulladb/remote` also enables SQLite, while `hulladb/hosting` enables the
backend's optional HTTP server stack.

Five crates own the Rust implementation: `hulladb`, `hulladb_schema`,
`hulladb_postgres`, `hulladb_macros` and the separate `hulladb_js` worker.
The shared schema crate owns function protocol and tagged record encoding too.
The four other published names remain thin compatibility entrypoints; they are
absent from the SDK's normal dependency graph. No registry dependency versions
were changed by the consolidation.

Unused futures procedural macros and syn extra traits were removed. Multithreaded
Tokio and process/signal features are enabled by executables or tests where used,
rather than forced by the generic SDK. Certificate generation stays in dev
dependencies. HTTP, TLS, cryptography, SQLite and TypeScript type analysis remain
maintained implementations; replacing these with handwritten versions would add
substantial correctness and maintenance risk. Small protocol and retention helpers
use existing standard/runtime APIs without new packages.

## Advisory check

On October 19, 2026, `bun audit --json` reported no advisories. An OSV batch query
of all resolved crates.io name/version pairs found four advisories on rustls
0.23.45, fixed by updating to 0.23.46:
[RUSTSEC-2026-0346](https://rustsec.org/advisories/RUSTSEC-2026-0346.html),
[RUSTSEC-2026-0347](https://rustsec.org/advisories/RUSTSEC-2026-0347.html),
[RUSTSEC-2026-0348](https://rustsec.org/advisories/RUSTSEC-2026-0348.html), and
[RUSTSEC-2026-0349](https://rustsec.org/advisories/RUSTSEC-2026-0349.html).
Their triggering configurations differ; this does not imply every advisory was
reachable through this package. The subsequent OSV scan of all 202 resolved
registry versions reported no known advisories. Advisory databases change; repeat
these checks when resolving dependencies or preparing a release.

The October 20 calendar/codec work promotes the already resolved `time 0.3.55`
into the schema/PostgreSQL production graph for Gregorian validation and typed
driver I/O. Parsing/formatting features are explicit; application timezone
conversion never reads the machine's local timezone. The PostgreSQL driver uses
its maintained `with-time-0_3` integration rather than a new binary date codec.
A scoped check of [RustSec's time advisories](https://rustsec.org/packages/time.html)
found the resolved version outside both published affected ranges:
[RUSTSEC-2026-0009](https://rustsec.org/advisories/RUSTSEC-2026-0009) is patched in
0.3.47 and [RUSTSEC-2020-0071](https://rustsec.org/advisories/RUSTSEC-2020-0071)
in 0.2.23. This is a check of the changed dependency, not a new workspace audit.

The schema-adapter work adds optional peer/export metadata for the existing
resolved Zod 4.6.7, without adding resolved dependency versions. Its scoped
[Zod advisory check](https://github.com/advisories/GHSA-m95q-7qp3-xv42) found that
version outside the advisory's affected range (up to 3.22.2; patched in 3.22.3).
This is not a full dependency audit.

The October 22 hosted proof replaces the custom PostgreSQL TLS adapter with
`tokio-postgres-rustls 0.14.0` for SCRAM channel binding. Certificate verification
still uses rustls, the explicit ring provider and WebPKI roots. Its certificate
parser (`x509-cert`) and hashing dependencies implement the standardized binding;
no handwritten certificate parser or cryptography is introduced. A scoped OSV
check of the newly resolved adapter/parser dependencies reported no advisories.
The adapter is reachable from the service/direct bridge, and remains absent from
the browser and restricted handler process.

## Retained state

Disposable browser and native result caches are bounded; see the
[TypeScript SDK](../packages/db/README.md) and
[native remote client](rust-remote-client.md). Database bridge admission, execution
and shutdown are bounded as described in [server access](server-database.md).

Durable mutation intents, completed receipts and source/partition identities are
correctness state. Native applications consume and explicitly forget delivered
receipts. A source identity cannot be dropped merely because an auth scope became
inactive. Applications holding handles or arbitrary callback closures also own
those references. Cache eviction does not silently discard recovery evidence or
claim to bound memory retained by application code.
