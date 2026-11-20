# Release workflow

Install `@hulla/db` and its matching adapters from npm or `hulladb` from crates.io.
npm version preparation and OIDC publication use the
[automated npm release workflow](npm-releases.md). The steps below cover native
artifact preparation and the separate Cargo release process.
The Rust packages use version `0.1.1`. The Rust implementation preserves
public crate paths, features and formats. The npm packages include local development,
application schema upgrades and the optional `@hulla/db-studio` workbench.
Preparation creates
local artifacts; it never calls `npm publish`, `cargo publish`, pushes Git commits,
creates hosted services or enables paid features.

For the Rust-only patch, use:

```sh
bun run release:prepare:cargo
bun run release:check:cargo
```

This produces nine Cargo archives and their manifest/checksums in
`output/releases/0.1.1/`, without rebuilding or republishing npm. Five crates own
the implementation; four preserve published imports through reexports. Full
release preparation also supports different Rust/npm versions: archive filenames
carry their own registry version, and the manifest directory uses the Rust version.

## Build the candidate

From the repository root:

```sh
bun install --frozen-lockfile
bun run release:prepare
bun run release:check
```

Stage genuine native binaries first when they are missing or runtime source has
changed. Build the macOS arm64 executables on that host with
`bun run --cwd packages/db build:native`, then build the optional local
PostgreSQL assets and the SDK packages:

```sh
bun run --cwd packages/db build:postgres
bun run build
bun packages/db/tests/development.integration.ts
```

`build:postgres` builds the checksum-pinned official PostgreSQL 18.6 source on the
target host using its C compiler and Make. It also accepts `--source-archive <file>`
for an offline source archive, with the same checksum verification. The resulting
bundle contains the server, initialization tool, runtime libraries and data files,
without optional system libraries. On macOS it rewrites and signs library references
so assets remain usable after leaving the build directory. The npm build pins
the size and checksum of each platform archive in the CLI. The CLI uses the
installed Bun runtime and its own pinned TypeScript dependency.

`packages/db/scripts/postgres-build.json` identifies the managed PostgreSQL build.
Increment its revision when the build recipe or target baseline changes, and set
the version and source checksum when changing the upstream PostgreSQL release.
Keep published builds available for the SDK releases that pin them. To validate a
candidate with an existing build, use `HULLADB_REUSE_POSTGRES_ASSETS=true bun run
release:check:npm`.

For Linux x64, build and stage the
genuine cross-platform assets:

```sh
docker build --platform linux/amd64 -f packages/db/Dockerfile.native \
  --output type=local,dest=/tmp/hulladb-native .
bun run --cwd packages/db build:native --from /tmp/hulladb-native/native --platform linux-x64
mkdir -p packages/db-cli/native/linux-x64
cp -a /tmp/hulladb-native/local-postgres/. packages/db-cli/native/linux-x64/
```

The release requires both `darwin-arm64` and `linux-x64`; Linux uses glibc.
The Linux runtime and development assets use Debian 12 (Bookworm) as their build
baseline so local PostgreSQL also runs on that distribution.
Other native targets need their own build and execution evidence before inclusion.
Browser bindings do not execute native binaries.
The installed development check must run on each released native target. It
exercises generation, Date codecs, hot reload, persistence, schema mismatch
recovery, external database ownership and process cleanup. It uses a loopback
download server to exercise checksum rejection, retry and offline cache reuse.

For a full release, artifacts are written under `output/releases/<rust-version>/`: the configured npm tarballs, nine Cargo
archives, `manifest.json` and `SHA256SUMS`. The manifest records exact archive
hashes, package sizes, source HEAD and whether the checkout was dirty. The two
managed PostgreSQL downloads are staged under `output/releases/<npm-version>/`.
Their checksums are pinned in the CLI tarball. Treat a
dirty candidate as review material: commit the reviewed source, rerun preparation
and checks, and tag the exact release commit before registry publication.

The npm builder stages production manifests without workspace-only source export
conditions, dev dependencies or build scripts. Package workspace manifests declare
public npm access. Publish the prepared tarballs,
not the `packages/*` source directories. Every tarball contains MIT license text.
Each platform dependency of `@hulla/db-postgres` includes its native dependency
license and copyright notices.
If a registry dependency omitted its shared license, the builder recovers
it from the immutable upstream commit recorded by Cargo. That step needs network
access and fails if the original notice cannot be recovered.

Cargo packaging compiles each production archive, using Cargo's temporary registry
for unpublished sibling dependencies. Internal path dependencies carry registry
versions. The diagnostic `hulladb_cdc` crate remains unpublished. Rust tests that
require the sibling npm checkout are omitted from the published archives; they
remain available in the repository. Private unit-test modules remain included
because Rust library tests reference them.

`release:check` installs the npm archives into a disposable directory outside the
checkout. It uses npm to resolve the unpublished sibling tarballs, generates an
application through the installed CLI, compiles all framework/Better Auth consumers,
checks the browser bundle and license files, and runs the packaged native processes
under Node without checkout/PATH fallbacks. It also runs npm publication dry runs
for all configured archives without uploading them. The temporary consumer is removed
whether checks pass or fail. It also extracts the nine Cargo archives outside the
checkout and checks every target/feature against those extracted sibling packages,
using cached registry dependencies. This is archive validation; it does not replace the
SDK or PostgreSQL behavior tests recorded in the implementation/deployment notes.

## Publication order

Publish npm core first, followed by the optional drivers, Studio and framework/auth integrations:

```text
@hulla/db
@hulla/db-postgres-darwin-arm64
@hulla/db-postgres-linux-x64
@hulla/db-postgres
@hulla/db-sqlite
@hulla/db-react
@hulla/db-studio
@hulla/db-cli
@hulla/db-vue
@hulla/db-svelte
@hulla/db-solid
@hulla/db-better-auth
```

Each staged npm manifest specifies public access. Inspect an archive without
uploading it with `npm publish --dry-run --ignore-scripts /absolute/package.tgz`.
Actual publication uses `npm publish /absolute/package.tgz --access public` after
release approval and registry authentication.

Cargo dependencies, including development dependencies needed during package verification,
must be available before their consumers:

```text
hulladb_schema
hulladb_macros
hulladb_js
hulladb_protocol
hulladb_wire
hulladb_postgres
hulladb_server
hulladb
hulladb_client
```

Wait for each dependency to be indexed before publishing consumers. Run
`cargo publish -p <crate> --locked` from the reviewed release checkout. Cargo
repackages source at publication, so retain and compare the candidate checksums
and source commit. `cargo package --workspace --exclude hulladb_cdc --all-features`
validates the unpublished dependency set without uploading it.

Verify that the source commit is pushed to the public repository and every registry
version is accessible without authentication. Install the published package names in
a fresh consumer; local archive checks alone do not establish registry publication.

## Compatibility policy

This is the first public storage and protocol baseline. Deploy the service, SDKs
and generated artifacts from the same exact release; mixed-release deployments
are unsupported unless explicitly documented. The Rust `0.1.1` consolidation is
compatible with the `0.1.0` storage/protocol baseline; no migration
or regenerated application artifacts are required. There is no supported upgrade
path from development fixtures.

Existing `hulladb_client`, `hulladb_protocol`, `hulladb_wire` and `hulladb_server`
imports remain supported. They forward to the canonical implementations in
`hulladb` (HTTP transport), `hulladb_schema` (protocol/wire), and
`hulladb_postgres` (HTTP hosting). The server compatibility package also retains
the `hulladb-bridge` and `hulladb-service` executable names and `typescript` feature.
Macros and the isolated JS worker retain their separate compiler/process boundaries.

Initialization verifies an existing schema and refuses incompatible storage; it
does not reset data or run migrations. Back up existing data before deployment.
A separate explicit [application schema upgrade](schema-migrations.md) can add
tables and nullable/defaulted fields within this storage baseline. Ship matching
native executables and SDK artifacts containing the migration command.
A future release that changes persisted server/client data or the protocol must
provide an explicit upgrade/recovery plan before publication, including required
privileges, destructive steps, rollback guarantees and mixed-version handling.
