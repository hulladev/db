# Hulla DB

A Rust database platform with PostgreSQL as its first authoritative backend and
planned Rust and TypeScript SDKs. The [architecture proposal](docs/architecture.md)
describes the intended behavior and distinguishes requirements from open decisions.

The npm package will be `@hulla/db`. Its Rust SDK counterpart will use the Cargo
package and import name `hulladb`. Supporting crates use the `hulladb_` prefix;
the current contract crates are `hulladb_schema` and `hulladb_wire`.
The SDK packages will be created in later implementation chunks.

Implementation proceeds in small chunks, with a review at the end of each chunk.
The first chunk establishes development tooling. The second implements internal
table contracts and primary-key normalization. The third resolves key generation
and insert policy. The fourth validates decoded row and insert values, keeping
nullability separate from omission. The fifth adds validated literal field
defaults and insert preparation. The sixth implements partial updates with
immutable primary keys and indexed field lookups. The seventh resolves explicit
references across normalized tables, checking target keys and rejecting invalid
or ambiguous declarations. References require integrity and restricted deletion;
enforcement inside storage transactions remains future work. The eighth restricts
primary keys to one UUID, string, or signed 64-bit integer field and rejects boolean
keys. Composite keys remain future scope. The ninth adds a schema-wide UUIDv4/v7
generation default, with explicit table overrides taking precedence. These remain
generation descriptors; generator execution is later runtime work. The tenth adds
named single-field and multi-field unique constraints, rejects malformed or
duplicate declarations, and requires explicit null semantics for nullable unique
fields. Uniqueness enforcement is later storage work. The eleventh adds a lossless
JSON record codec, with tagged integers/UUIDs and duplicate field detection.
Server, SDK, native authoring frontends, transport protocols, and database adapters
are later chunks.

## Prerequisites

- Rust through [rustup](https://rustup.rs/). The workspace pins
  [Rust 1.98.1](https://blog.rust-lang.org/2026/09/03/Rust-1.98.1/),
  rustfmt, and Clippy in `rust-toolchain.toml`.
- Make to run the convenience commands; the underlying Cargo commands also work
  directly.
- Docker with Compose v2 or newer for the optional local PostgreSQL instance.
  OrbStack or Docker Desktop can provide the engine on macOS.

TypeScript tooling is not required for this chunk. We will select it when adding
the TypeScript authoring frontend or SDK. SQLite tooling will be selected for the
early storage adapter spike.

## Rust development

```sh
rustup toolchain install 1.98.1 --profile minimal --component rustfmt --component clippy
make check
```

`make check` checks formatting, runs Clippy with warnings treated as errors, and
builds/runs workspace tests and documentation tests. It does not require a database.
The schema crate has integration tests for table/reference normalization, key
configuration, and value validation. The wire crate tests exact JSON encoding,
decoding failures, and integration with schema validation. Both include executable
documentation examples. Use `make fmt` to
format edits.

Commit `Cargo.lock` when version control is initialized. It will keep dependency
resolution reproducible as the workspace gains service and development binaries.
The packages are unpublished, and a license has not yet been selected.

## Local PostgreSQL

```sh
make db-config
make db-up
make db-shell
```

The development database uses `postgres:18.6-bookworm`, listens on
`127.0.0.1:55432`, and has database, username, and password `db_dev`.
These credentials are for local development only. The connection URL is:

```text
postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev
```

`make db-up` waits for PostgreSQL readiness. `make db-shell` uses the container's
`psql`, so no host PostgreSQL installation is required. A host client can connect
using the URL above. If the port is occupied, use `DB_PORT=55433 make db-up` and
adjust the connection URL accordingly.

`make db-down` stops and removes this project's container and network, preserving
the named data volume. Startup does not install application tables or migrations.
Rust checks do not start PostgreSQL automatically.

The volume mounts `/var/lib/postgresql`, following the
[PostgreSQL 18+ image layout](https://hub.docker.com/_/postgres).
Changing the image to a different PostgreSQL major version requires an explicit
database upgrade; changing the tag alone is insufficient.

## Codebase map

| File or directory | Responsibility |
| --- | --- |
| `docs/architecture.md` | Product requirements, proposed boundaries, acceptance gates, and open decisions |
| `docs/wire-format.md` | Initial JSON record/scalar encoding and decoding contract |
| `Cargo.toml` | Rust workspace, common package settings, and shared lint policy |
| `rust-toolchain.toml` | Exact Rust compiler and development components |
| `crates/hulladb_schema/src/types.rs` | Initial logical scalar types, field defaults, and shared scalar validation |
| `crates/hulladb_schema/src/table.rs` | Table declarations, normalization, field-name index, and resolved primary keys |
| `crates/hulladb_schema/src/schema.rs` | Schema normalization, indexed table lookup, and explicit reference resolution |
| `crates/hulladb_schema/src/key.rs` | UUID default selection, key generation descriptors, and insert-key policies |
| `crates/hulladb_schema/src/unique.rs` | Named unique constraints, explicit null semantics, and structural normalization |
| `crates/hulladb_schema/src/error.rs` | Structured schema diagnostics |
| `crates/hulladb_schema/src/value.rs` | Decoded logical values and named field values |
| `crates/hulladb_schema/src/validation.rs` | Row/insert/update validation, default application, in-memory updates, and value diagnostics |
| `crates/hulladb_schema/tests` | Contract scenarios for normalization, generators, defaults, updates, and references |
| `crates/hulladb_wire` | Lossless JSON record codec and wire diagnostics; depends on logical schema values |
| `Makefile` | Repeatable local checks and database commands |
| `compose.yaml` | Optional local PostgreSQL service and persistent data volume |

We will introduce additional crates when they have concrete responsibilities and
code. The first crate will hold the canonical contract produced by native Rust or
TypeScript schema declarations; it is not a hand-authored schema file format.

## Next review boundary

The [schema contract discussion](docs/schema-contract.md) records agreed principles
and remaining decisions. Omitting `id` from a table declaration adds a UUID field
with the schema's UUID generation default; the primary key defaults to `id` even when its type is
explicitly overridden. Selecting another primary key suppresses implicit `id`.
The normalizer resolves the schema UUID default for UUID keys, including declared
UUID fields. Set `SchemaDeclaration::default_uuid_generation` to `UuidGeneration::V4`
or `V7`; v7 is the unconfigured and standalone-table default. Explicit per-table
`key_generation` settings always take precedence, including disabled generation.
String and integer keys have no default generator: inserts must supply their keys unless
a compatible generator is configured. Explicit keys are accepted by default;
`GeneratedOnly` requires a generator. UUIDv4, custom handler descriptors, and
explicitly disabling generation are supported as configuration. No key values are
produced yet. Decoded insert values now enforce that policy and report whether
the runtime must generate a key.

Complete rows require all fields. Nullable fields accept explicit null but still
require presence on insert unless a literal default is configured. Normalization
rejects defaults that do not match their field. `prepare_insert()` validates and
fills omitted defaults; supplied values are never replaced. Unknown fields,
forbidden nulls, and scalar type mismatches are rejected without coercion. Primary
keys continue to use key generation rather than field defaults.

Partial updates leave omitted fields unchanged, clear nullable fields with
explicit null, and validate replacement values. The selected primary key must
be omitted. `apply_update()` validates a complete row and its patch before making
any in-memory changes; defaults and generators are never applied during updates.

Schema normalization now resolves explicit references to selected primary keys
and rejects missing targets, type mismatches, and duplicate declarations. Target
existence and restricted deletion must be enforced inside authoritative storage
transactions; the current schema crate cannot check stored target rows.

Primary keys now select exactly one UUID, string, or signed 64-bit integer field.
Boolean fields remain supported as data fields, but cannot be selected as keys.
Composite primary keys are deferred; their components are never silently reduced
to a single field or joined into a string.

Tables now declare named `UniqueConstraintDeclaration` values for single-field or
multi-field business uniqueness, such as `(projectId, userId)`. Normalization
checks names and fields and rejects repeated field sets regardless of order.
Constraints containing nullable fields require an explicit `UniqueNullPolicy`:
`Distinct` permits repeated null-containing tuples, while `NotDistinct` treats
null as equal to null. Row validators do not check other stored rows; adapters must
enforce uniqueness atomically, including during concurrent writes and migrations.

`hulladb_wire::decode_record()` now parses JSON records without discarding duplicate
field names. Strings, booleans, and null are native JSON; integers use `$int64`
decimal-string tags and UUIDs use `$uuid` hyphenated-string tags. Native JSON numbers
are rejected. Encoding preserves the complete signed 64-bit range and UUID bytes.
The codec does not apply schema defaults or verify table shape; callers use the
existing schema validators afterward. See the [wire format](docs/wire-format.md).

The next review boundary is explicit projection contracts for partial query
results. Transport envelopes/versioning, dynamic field defaults, stable schema identities,
composite keys, and additional relationship policies
remain subsequent contract chunks.

Public API spelling and the two native authoring frontends remain provisional.
The development database version is a starting baseline, not a promise that every
PostgreSQL version is already supported.
