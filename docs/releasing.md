# Release preparation

The first release is `0.1.0`, under MIT, from
[the hulladev/db repository](https://github.com/hulladev/db). Preparation creates
local artifacts; it never calls `npm publish`, `cargo publish`, pushes Git commits,
creates hosted services or enables paid features.

## Build the candidate

From the repository root:

```sh
bun install --frozen-lockfile
bun run release:prepare
bun run release:check
```

Stage genuine native binaries first when they are missing or runtime source has
changed. Build the macOS arm64 executables on that host with
`bun run --cwd packages/db build:native`. For Linux x64, use the Docker build and
staging command in [deployment](deployment.md#build-an-installable-package).
The first release requires both `darwin-arm64` and `linux-x64`; Linux uses glibc.
Other native targets need their own build and execution evidence before inclusion.
Browser bindings do not execute native binaries.

Artifacts are written to `output/releases/0.1.0/`: six npm tarballs, nine Cargo
archives, `manifest.json` and `SHA256SUMS`. The manifest records exact archive
hashes, package sizes, source HEAD and whether the checkout was dirty. Treat a
dirty candidate as review material: commit the reviewed source, rerun preparation
and checks, and tag the exact release commit before registry publication.

The npm builder stages production manifests without workspace-only source export
conditions, dev dependencies or build scripts. Source workspace manifests stay
private to prevent accidental directory publication. Publish the prepared tarballs,
not the `packages/*` source directories. Every tarball contains MIT license text;
the core archive additionally preserves native dependency license/copyright
notices. If a registry dependency omitted its shared license, the builder recovers
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
for all six archives without uploading them. The temporary consumer is removed
whether checks pass or fail. It also extracts the nine Cargo archives outside the
checkout and checks every target/feature against those extracted sibling packages,
using cached registry dependencies. This is archive validation; it does not replace the
SDK or PostgreSQL behavior tests recorded in the implementation/deployment notes.

## Publication order

Publish npm core first, followed by the five peer integrations:

```text
@hulla/db
@hulla/db-react
@hulla/db-vue
@hulla/db-svelte
@hulla/db-solid
@hulla/db-better-auth
```

Each staged npm manifest specifies public access. Inspect an archive without
uploading it with `npm publish --dry-run --ignore-scripts /absolute/package.tgz`.
Actual publication uses `npm publish /absolute/package.tgz --access public` after
release approval and registry authentication.

Cargo dependencies must be available before their consumers:

```text
hulladb_schema
hulladb_protocol
hulladb_macros
hulladb_js
hulladb_wire
hulladb_client
hulladb_postgres
hulladb_server
hulladb
```

Wait for each dependency to be indexed before publishing consumers. Run
`cargo publish -p <crate> --locked` from the reviewed release checkout. Cargo
repackages source at publication, so retain and compare the candidate checksums
and source commit. `cargo package --workspace --exclude hulladb_cdc --all-features`
validates the unpublished dependency set without uploading it.

The repository was created privately during preparation. Make its reviewed source
public before an open-source launch, then verify that repository/readme links are
accessible. Package-name availability checks do not reserve registry names, and
preparation does not prove publishing credentials or ownership of an npm scope.
