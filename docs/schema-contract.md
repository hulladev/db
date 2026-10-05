# Schema contract discussion

**Status:** Table normalization, single-field key type restrictions, key configuration, row/insert/update validation, literal defaults, in-memory updates, schema-level reference resolution, and unique constraint normalization implemented; storage enforcement and additional contract decisions remain open

This document records the small design steps preceding architecture phase 1.
API spelling and examples below remain illustrative.

## Agreed principles

- npm package: `@hulla/db`.
- Rust package and import: `hulladb`; supporting crates use the `hulladb_` prefix,
  beginning with `hulladb_schema`.
- `id` is optional in a table declaration. If omitted, the default table contract
  adds an `id` field with UUID type and the schema's UUID generation default.
  The schema default is UUIDv7 unless explicitly configured as UUIDv4.
- The primary key defaults to `id`, whether that field is implicit or explicitly
  declared. Declaring `id` with another supported validator changes its type and
  preserves that default primary-key selection.
- Initial primary keys select exactly one UUID, string, or signed 64-bit integer
  field. Boolean fields remain supported as ordinary data, but cannot be keys.
  Composite primary keys are deferred until references and SDK identities can
  represent them without loss or ambiguity.
- Developers can explicitly override the primary-key selection and override
  generation logic independently. UUIDs are not a mandatory identifier format.
- Selecting a different primary key suppresses implicit `id` injection. An
  explicitly declared `id` remains an ordinary field in that case.
- Each table controls whether inserts may supply an explicit key or must use its
  configured generator.
- UUID keys inherit the schema's UUIDv4 or UUIDv7 generation default unless a
  table overrides it. Standalone table normalization defaults to UUIDv7.
  String and integer keys without an
  explicit generator require caller-supplied values.
- Inserts accept explicit keys by default and generate omitted keys when a
  generator is configured. Requiring generation is an explicit table override.
- Developers describe fields using declarative validator descriptors. The same
  declaration supports runtime validation and generated SDK types.
- Developers explicitly define relationships. A field name such as `projectId`
  does not by itself establish a relationship.
- References require referential integrity: non-null values must point to existing
  rows in the target table. Deletion is restricted while dependent references
  exist. The initial contract has no unenforced-reference option or automatic
  cascade/set-null behavior.
- Authoritative writes must preserve ACID and enforce reference constraints
  atomically with the write. Schema and SDK validation do not replace database
  constraints or transaction isolation.
- Nullability permits an explicit `null`; it does not permit omission on insert.
  Ordinary fields require values unless an explicit default is configured.
  Literal default descriptors and insert preparation are implemented.
- Complete rows contain every normalized field. Partial updates distinguish
  omission (unchanged) from explicit null (clear); insert defaults will not apply
  to omitted update fields.
- The selected primary key is immutable in ordinary updates. An update cannot
  supply that field, even with its current value. Any future operation to change
  keys requires an explicit contract for relationships and synchronization.

The architecture proposes native Rust or TypeScript authoring compiled to one
language-neutral contract, with one authoritative source per project. The contract
will describe the authored schema to SDK generators and storage adapters.

## Implemented slice

The `hulladb_schema` crate currently implements the internal table model and
structural normalization. Its initial scalar descriptors are string, boolean,
signed 64-bit integer, and UUID, with field nullability represented separately.
This subset will grow as validator semantics are agreed.

`TableDeclaration::normalize()` resolves the primary key and validates names,
defaults, and duplicates. It rejects missing custom key fields, nullable primary
keys, and unsupported key types, and injects the implicit ID when applicable.
Declared field order is preserved;
injected `id` is appended. A `TableContract` provides read-only access to the
resolved fields and primary key. Normalization also builds a field-name index,
preserving declared field order while avoiding linear scans for name lookups.

Normalization resolves key generation and insert policy. UUID primary keys inherit
`SchemaDeclaration::default_uuid_generation`, whether injected or declared and
regardless of the selected field's name. `UuidGeneration::V7` is the unconfigured
default; `V4` selects UUIDv4. Standalone `TableDeclaration::normalize()` uses v7.
String and integer keys receive `KeyGeneration::None`,
meaning that insert keys must be supplied. Boolean key fields are rejected before
defaults and generator configuration are checked.

Generation can be overridden with UUIDv4, UUIDv7, a custom handler descriptor, or
explicitly disabled. The generator's declared output type must match the selected
key's scalar type. Empty custom handler names and `GeneratedOnly` without a
generator are schema errors. Handler registration and generation execution remain
later compiler/runtime stages; produced values must pass row validation.

Normalization produces no row values and does not make an unresolved schema
deployable. Adapter capability restrictions, source locations, stable
identities, transport envelopes, relationship execution, and generated SDK types remain later
contract/compiler steps.

Executable Rust documentation and integration tests exercise the agreed
normalization behavior. The Rust structures are internal compiler descriptors;
native Rust and TypeScript authoring syntax is not implemented yet.

## Initial value validation

`Value` represents decoded string, boolean, signed 64-bit integer, UUID, or null
values. `RecordValues` maps field names to values; a missing map entry is omission,
which is different from `Value::Null`. UUIDs are represented by 16 bytes and
integers by `i64`. These are internal logical values, not a wire encoding or a
public SDK API. The separate `hulladb_wire` crate now supplies the initial
[JSON codec](wire-format.md): tagged decimal integers and hyphenated UUID text,
with duplicate field rejection before map entries can be overwritten. Additional
SDK/transport decoders must preserve the same logical guarantees.

`TableContract::validate_row()` requires every normalized field, including the
primary key and nullable fields. It rejects unknown fields, forbidden nulls, and
scalar type mismatches without coercion. Nullability does not bypass validation
of a non-null value's type. Errors carry table, field, and a structured reason.

Validation walks declared fields once and looks up their supplied values. After
all expected values validate, matching field counts prove there are no unknown
names, since the input map cannot contain duplicates. An extra-name scan runs
only when extra entries remain. Each omitted defaulted field and an omitted
generated key reduce the expected supplied-value count by one. Insert-key policy
errors are reported first, followed by value errors in schema order, then unknown
names in lexicographic order.

`TableContract::validate_insert()` enforces the same scalar rules, permits omission
of fields with literal defaults, and applies the resolved insert-key policy.
It returns `InsertKeyAction::Provided`
for a validated supplied key or `Generate` for an omitted key with a configured
generator. Null is a supplied value, not a request for generation. A missing key
without a generator and any supplied key under `GeneratedOnly` are rejected.

The runtime must fill defaults through insert preparation, execute pending key
generation, add the produced key, and validate the complete row before storage.
Neither validator modifies the input or executes a generator. Successful
validation does not prove uniqueness, authorization,
relationship existence, or compatibility with a storage adapter.

Updates use the partial-update contract below. Projections remain a separate
contract; the complete-row validator must not treat a partial record as complete.

## Partial updates

The internal `RecordValues` map represents three distinct field intents: absence
means unchanged, `Value::Null` means clear a nullable field, and a scalar value
means replace. This is an internal decoded representation; generated Rust patch
types and the TypeScript API remain later authoring/SDK work.

`TableContract::validate_update()` checks only supplied fields, using the
normalized table's field-name index. It rejects unknown names, forbidden nulls,
and scalar mismatches without coercion. It rejects any supplied selected primary
key before checking other fields, including assignment of its current value.
An ordinary field named `id` can still be updated when another field is the key.
An empty update is valid. Defaults and generators never run during updates.
Other update errors are reported in lexicographic field-name order.

`TableContract::apply_update()` takes an existing complete row by mutable borrow
and consumes a partial update. It validates the complete row and the update before
modifying the row, then moves the supplied values into it. A validation error
leaves the row unchanged. A malformed base row is rejected, even if the patch
could repair it; missing stored fields are never filled from insert defaults.

This helper applies logical values in memory. It does not prove target existence,
authorization, storage uniqueness, or relationship integrity, and it is not a
storage transaction or concurrency policy. Future adapters must enforce the
architecture's strict-update rule and transaction boundaries.

## Literal field defaults

`FieldDefinition::default` is an `Option<Value>`: `None` means no default, while
`Some(Value::Null)` explicitly supplies null when a nullable field is omitted.
String, boolean, signed 64-bit integer, and UUID literals are also supported.
Normalization validates defaults against their field's scalar type and
nullability using the same value check as row and insert validation. Invalid
defaults are schema errors, even when a particular insert would supply a value.

`TableContract::prepare_insert()` consumes an input map, validates it, and returns
a `PreparedInsert` with omitted literal defaults filled in and its pending
`InsertKeyAction`. The schema is traversed once; missing defaults are collected
as borrowed references and cloned into the owned input map only after validation
succeeds. Existing input values are moved with the map rather than copied.
`validate_insert()` remains a read-only check; it permits defaulted omissions
without producing a prepared row. Callers can use `prepare_insert()` directly,
without separately repeating insert validation.

Defaults never replace supplied values. Explicit null is accepted only on nullable
fields and does not invoke a default. Complete-row validation always requires
presence, even for defaulted fields. Update omission will mean unchanged, so it
will not invoke insert defaults either.

The selected primary key cannot have a field default in this implementation
slice; key generation is configured separately, preserving its insert policy.
An ordinary field named `id` can have a default when another field is selected
as the primary key. Function, time-dependent, and server-handler field defaults
require a later contract for execution and retries; they are not implemented.

## Initial primary-key contract

Every table has exactly one non-null primary-key field. The initial supported
types are UUID, string, and signed 64-bit integer; `UnsupportedPrimaryKeyType`
rejects boolean keys. A boolean named `id` remains valid as an ordinary field when
another supported field is selected as the key. This restriction follows the
selected field, not its name, and applies inside schema-level normalization too.

This is an initial implementation scope and API policy, not a requirement for
ACID or a universal SQL limitation. A non-null unique boolean key can be valid
but permits at most two rows. Additional key types need explicit equality,
lossless wire values, SDK identities, and adapter mappings; the allowlist should
be revisited as those capabilities are implemented.

The existing `primary_key: Option<String>` selects a single exact field name. It
does not encode a field list, concatenate multiple values, or substitute one part
of a composite key. Composite primary keys are not represented by this slice.
Future authoring frontends must reject unsupported composite declarations explicitly.

This keeps the initial row identity consistent across references, updates, and
future subscriptions. Composite keys are valid relational designs, but require
an explicit tuple representation and a complete contract for references,
generation, wire values, SDK types, and migrations before support is introduced.

A single row ID does not enforce business uniqueness across other fields. For
example, a membership table may need both an `id` and a unique constraint on
`(projectId, userId)`. Two different generated IDs do not prevent duplicate
memberships. Unique constraints, including multi-field constraints, are a separate
contract now described below. PostgreSQL distinguishes these constraints from
the choice of primary key; see [constraints](https://www.postgresql.org/docs/18/ddl-constraints.html).

### Identity and adapter requirements

- Preserve UUID values as their exact 16 bytes; UUIDv7 generation does not imply
  that every supplied UUID must have version 7.
- Preserve the full signed `i64` range, including negative values and zero. Do
  not round through a JavaScript number, reinterpret it as unsigned, or silently
  allocate an integer key when the contract requires a supplied value.
- Preserve string identity exactly. No implicit trimming, case folding, Unicode
  normalization, or numeric conversion is performed by the logical contract.
  Adapters must use compatible equality semantics for keys and their references.
- Preserve non-null keys, immutability in ordinary updates, and storage uniqueness.
  Generator descriptors neither prove uniqueness nor execute generation.
- Reject values or schemas an adapter cannot represent faithfully before any
  command changes commit. Logical scalar validation alone does not prove adapter
  compatibility. Backend limits and portable string-value restrictions remain
  later adapter/value work. PostgreSQL `text`, for example, cannot store a NUL
  character; see [character types](https://www.postgresql.org/docs/18/datatype-character.html).

SQLite's `STRICT` mode still performs some lossless coercion, and inserting NULL
into `INTEGER PRIMARY KEY` can allocate an integer. An adapter must enforce Hulla's
explicit value and generation policy rather than inherit these behaviors;
see [SQLite STRICT tables](https://www.sqlite.org/stricttables.html). Actual DDL,
collation, driver bindings, and adapter conformance tests remain future work.

## Unique constraints

`TableDeclaration::unique_constraints` contains named
`UniqueConstraintDeclaration::new(name, fields)` values. A constraint requires
the selected tuple to be unique across the table: `(projectId, userId)` prevents
duplicate memberships without making either field individually unique. The
table retains its single-field primary key. All current scalar types, including
boolean, support business uniqueness descriptors.

```rust
use hulladb_schema::{FieldDefinition, ScalarType, TableDeclaration, UniqueConstraintDeclaration};

let mut memberships = TableDeclaration::new("memberships", vec![
    FieldDefinition::new("projectId", ScalarType::Uuid),
    FieldDefinition::new("userId", ScalarType::Uuid),
]);
memberships.unique_constraints.push(UniqueConstraintDeclaration::new(
    "project_user", vec!["projectId".into(), "userId".into()],
));
let memberships = memberships.normalize().unwrap();
// Primary key: id. Separate business constraint: (projectId, userId).
```

Normalization runs after key/default/generator validation and implicit ID
injection. It rejects empty names, duplicate names within a table, empty field
lists, missing fields, repeated fields within a constraint, and repeated field
sets across constraints. Reordering the same fields does not create a distinct
uniqueness requirement; duplicate field sets are rejected even with different
null policies. Overlapping field sets remain valid. Constraint names may be
reused by different tables. Names are logical diagnostic labels, not physical
SQL identifiers or stable migration identities.

`TableContract::unique_constraints()` exposes read-only contracts with the name,
ordered field names, and resolved null policy. Declaration and field order are
preserved for adapter planning. Fields resolve by exact name against the
normalized table, including injected `id`; missing fields are never injected on
behalf of a unique constraint. Existing reference declarations continue to target
the selected primary key, not an alternate unique field or tuple.

### Explicit null semantics

If any selected field is nullable, the declaration must set `null_policy` to
`Some(UniqueNullPolicy::Distinct)` or `Some(UniqueNullPolicy::NotDistinct)`.
Omission is a `MissingUniqueNullPolicy` error, even if a non-null default is
configured. A default does not prevent an explicit null.

| Policy | Conflict rule | Example for `(projectId, userId)` |
| --- | --- | --- |
| `Distinct` | A tuple containing any null never conflicts | Repeated `(projectA, null)` values are allowed |
| `NotDistinct` | All components must compare equal, with null equal to null | Repeated `(projectA, null)` values conflict; `(projectB, null)` remains distinct |

For a single nullable email field, `Distinct` allows multiple users without an
email, while `NotDistinct` allows at most one null email. Entirely non-null
constraints may omit the policy; normalization resolves it to `NotDistinct`,
which has the same effect as `Distinct` when null is forbidden. Either explicit
policy is also accepted for non-null fields.

PostgreSQL defaults to distinct nulls and supports `NULLS NOT DISTINCT`;
SQLite's ordinary unique constraints treat nulls as distinct. The logical
contract requires the chosen behavior rather than inheriting an adapter default.
See [PostgreSQL unique constraints](https://www.postgresql.org/docs/18/ddl-constraints.html#DDL-CONSTRAINTS-UNIQUE)
and [SQLite unique constraints](https://www.sqlite.org/lang_createtable.html#unique_constraints).

### Required storage guarantees

This chunk validates descriptors only. `validate_row()`, `validate_insert()`,
`prepare_insert()`, and `apply_update()` do not consult other rows and cannot
detect a uniqueness violation. Nullable, omission, default, and key rules remain
unchanged. Repeated defaults and generated keys still require storage checks.

Future adapters must:

- Enforce the complete tuple on inserts and updates, including generated/defaulted
  values, under authoritative transaction isolation. An application-level lookup
  followed by an unconstrained write cannot prevent concurrent duplicates.
- Roll back the entire command on a conflict, including associated idempotency
  records and delivery-log entries; do not silently ignore, replace, or upsert a
  conflicting row. A future upsert operation needs its own explicit contract.
- Preserve exact scalar equality and the selected null policy. String uniqueness
  adds no trimming, case folding, or Unicode normalization. Adapters must reject
  unsupported equality/null semantics before schema activation or writes.
- Validate existing data when activating or changing a constraint. Conflicting
  rows require an explicit data migration; no automatic deletion or merging.
- Map storage conflicts to logical table/constraint diagnostics. Physical index
  names, null-policy implementation, and constraint-check timing remain adapter work.

Storage conformance tests must cover single-field and composite conflicts,
allowed tuples sharing only some values, nullable policies with null in each
position, updates, default/generated collisions, concurrent writers, command
rollback, and activation over conflicting existing rows. The current suite tests
declaration normalization; it does not establish these storage guarantees.

Partial/expression uniqueness, collations, alternate reference targets, physical
index planning, and stable constraint identities remain subsequent contracts.

## Default keys and generation overrides

The table contract needs to describe two separate choices:

| Choice | Describes | Example override |
| --- | --- | --- |
| Key schema | The primary-key field and its validator/type | Use a string key rather than the platform default |
| Key generation | How a new key is produced | Use an application-defined string generator |

The default primary-key field is `id`, its logical type is UUID, and its generator
produces UUIDv7 values unless the schema selects UUIDv4. A developer can keep this schema and replace its generator,
or override the schema as well. Overriding the type to string or integer defaults
to caller-supplied keys, with no automatic string or integer generator. A configured
generator must produce the selected key's type; incompatible combinations are
schema errors.

`SchemaDeclaration::default_uuid_generation` accepts only `UuidGeneration::V4`
or `V7`; it cannot accidentally select a non-UUID generator. Table-level
`key_generation: Some(...)` always takes precedence, including explicit disabling
with `Some(KeyGeneration::None)` and custom handler descriptors. The schema default
does not generate string or integer keys. `SchemaContract` retains the chosen
default and each normalized table contains its resolved generator.

```rust
use hulladb_schema::{KeyGeneration, SchemaDeclaration, TableDeclaration, UuidGeneration};

let projects = TableDeclaration::new("projects", vec![]);
let mut tasks = TableDeclaration::new("tasks", vec![]);
tasks.key_generation = Some(KeyGeneration::UuidV7);
let mut schema = SchemaDeclaration::new(vec![projects, tasks]);
schema.default_uuid_generation = UuidGeneration::V4;
// projects inherits v4; tasks explicitly uses v7.
```

These are internal compiler descriptors; native authoring configuration remains
provisional. Changing the default affects generation policy for future inserts,
not existing key values or target-reference types. Deployment still requires
compatibility/migration checks. Choosing v4 generation does not require all
supplied UUIDs to be v4, and choosing v7 does not require them to be v7.

When primary-key selection has not been explicitly overridden, normalization is:

| Authored declaration | Normalized key field | Primary key |
| --- | --- | --- |
| `id` omitted | Add `id` with UUID type and the schema UUID generator | `id` |
| `id` explicitly declared | Preserve its declared validator/type | `id` |

Illustrative declarations:

```ts
defineTable({ title: v.string() });
// Implicit id: UUID, generated as UUIDv7 unless the schema selects v4; primary key: id.

defineTable({ id: v.int64(), title: v.string() });
// Declared id: int64; primary key still: id.
// Inserts supply id unless a compatible generator is configured.
```

Omitting a field from `defineTable` is a schema-authoring convenience. The
normalized key field is present and non-null in committed rows. Omission from an
insert payload is governed separately by the table's generation/insert policy.

An explicit `primaryKey(...)` selecting a different field suppresses implicit `id`
injection. The selected field must be declared. Explicitly declared fields,
including `id`, are preserved. Explicitly selecting `id` uses the same injection
rule as the default selection.

Recommended generation boundary:

- Built-in generators have declarative descriptors. Custom generation refers to a
  registered server handler; compiler metadata does not attempt to serialize an
  arbitrary closure or translate it between Rust and TypeScript.
- Generated keys pass the declared validator and storage uniqueness constraints.
- Generation follows the platform's transaction and idempotency rules. Replaying
  a committed create recovers its existing result and key, rather than creating
  another record. Custom handlers must respect transaction retry restrictions.
- A custom server generator is not automatically available for client prediction.
  Optimistic creates need an explicitly supported prediction strategy; otherwise
  use server-confirmed execution.

The table's insert policy selects between two behaviors:

- Accept a caller-supplied key; generate one when the key is omitted, or reject
  omission if no generator exists. This is the default.
- Require generation; reject a caller-supplied key.

Explicit keys still pass the declared validator and uniqueness constraints.
Schema/generator overrides and per-insert key overrides are distinct. The internal
policy names are `InsertKeyPolicy::AllowExplicit` and `GeneratedOnly`; native
authoring API spelling remains provisional.

The resulting runtime contract is:

| Generator | Insert policy | Supplied key | Omitted key |
| --- | --- | --- | --- |
| None | AllowExplicit | Validate and accept | Reject: key required |
| Configured | AllowExplicit | Validate and accept | Generate and validate |
| Configured | GeneratedOnly | Reject: explicit key forbidden | Generate and validate |
| None | GeneratedOnly | Invalid schema | Invalid schema |

These rules are enforced on decoded insert values by `validate_insert()`;
generator execution remains runtime work. In `TableDeclaration`, `key_generation: None` selects the
type-based/schema default, while `Some(KeyGeneration::None)` explicitly disables
generation, including for UUID keys.

UUIDv7 is the unconfigured UUID generator. UUIDv4 can be selected schema-wide;
UUIDv4, UUIDv7, or custom generators can be selected per table.
Runtime execution of these descriptors is a later step. PostgreSQL
supports UUIDv4 and UUIDv7 generation; see its
[UUID functions](https://www.postgresql.org/docs/18/functions-uuid.html).
The portable platform generator must remain independent of the Postgres API.

## UUIDv7 portability and SDK typing findings

UUIDv7 generation and UUID storage are separate capabilities. The proposed
portable path is generation in Hulla's Rust core, with adapter-specific storage:
Postgres `uuid`, and a fixed 16-byte representation for SQLite-family adapters.
Native SQL generators can be optional adapter capabilities.

- [PostgreSQL 18](https://www.postgresql.org/docs/18/functions-uuid.html) provides
  `uuidv7()`. [PostgreSQL 17's built-in generator](https://www.postgresql.org/docs/17/functions-uuid.html)
  produces UUIDv4; its [UUID type](https://www.postgresql.org/docs/17/datatype-uuid.html)
  accepts externally generated UUIDv7 values.
- Stock SQLite's [UUID extension](https://raw.githubusercontent.com/sqlite/sqlite/master/ext/misc/uuid.c)
  generates UUIDv4. SQLite can store external UUIDv7 values as BLOB or TEXT;
  native SQL generation requires a suitable extension.
- The separate [Turso Database engine](https://github.com/tursodatabase/turso/blob/main/COMPAT.md#uuid)
  documents `uuid7()` with BLOB UUID values by default.
- libSQL/Turso Cloud documents a [SQLean UUID extension](https://docs.turso.tech/features/sqlite-extensions).
  SQLean's [extension source](https://raw.githubusercontent.com/nalgeon/sqlean/0.24.1/src/uuid/extension.c)
  exposes `uuid7()`. Adapters should verify the deployed extension capability,
  rather than infer support from the separate Turso engine.

Normal application-library generation is available in both SDK languages:

- Rust: `uuid::Uuid::now_v7()` with the `v7` feature; it returns a general `Uuid`.
  See the [uuid crate](https://docs.rs/uuid/latest/uuid/struct.Uuid.html#method.now_v7).
- TypeScript: `v7()` from `uuid`; the usual return value is a string.
  See [uuidjs](https://github.com/uuidjs/uuid) and its
  [return types](https://github.com/uuidjs/uuid/blob/main/src/v7.ts).

Recommendation: generate table-specific Rust newtypes and TypeScript branded
types, plus validated constructors. Validators check UUID syntax and an optional
version restriction; brands/newtypes preserve table association at compile time.
A raw UUID does not encode its table or prove a referenced row exists; existence
checks depend on the relationship enforcement policy. UUIDv7 generation need
not restrict every caller-supplied UUID to version 7 unless the declared validator
requires it. A custom primary-key type changes the generated key representation.

UUID timestamp ordering does not establish transaction commit ordering. Sync
continues to use the architecture's durable delivery-log cursor.

## Proposed representative example

Use two tables with different key types to exercise the chosen direction:

| Table | Primary key | Other fields |
| --- | --- | --- |
| Projects | `id`: string | `name`: string |
| Tasks | `id`: signed 64-bit integer | `projectId`: reference to Projects, `title`: string, `completed`: boolean, `notes`: nullable string |

Illustrative TypeScript authoring, not an implemented or stabilized API:

```ts
const schema = defineSchema({
  projects: defineTable({
    id: v.string(),
    name: v.string(),
  }),

  tasks: defineTable({
    id: v.int64(),
    projectId: v.ref("projects"),
    title: v.string(),
    completed: v.boolean(),
    notes: v.nullable(v.string()),
  }),
});
```

Recommendation: a reference derives its value representation from the target's
primary key. Here `projectId` refers to a string key; it does not impose UUIDs or
an unrelated key representation. Generated SDK types should retain table-specific
identities, so two tables with string keys cannot be mixed accidentally.

The example illustrates explicit `id` type overrides. Both tables use `id` as
their primary key without a `primaryKey(...)` call. Their string and integer keys
must be supplied on insert unless compatible generators are configured.

## Validator and relationship boundaries

Declarative validators describe supported types and portable constraints. An
arbitrary Rust closure or TypeScript callback does not automatically become a
portable validator, database constraint, or locally executable function.

References require target existence for every non-null stored value. Deleting a
referenced row is restricted; dependent rows must first be explicitly deleted or
reassigned, or their nullable references explicitly cleared. Null means no target
and requires a nullable source field. Automatic cascade and set-null policies
require a later explicit decision and a contract for authorization and atomic
change capture. There is no dangling-reference opt-out in this slice.

### Implemented reference resolution

`SchemaDeclaration` contains table declarations and explicit
`ReferenceDeclaration::new(source_table, source_field, target_table)` descriptors.
It normalizes every table first, then resolves each reference to the target's
selected primary key, including custom names and injected UUID keys. The source
field's declared scalar type must match that key exactly. Future native authoring
frontends can derive the source representation from `ref(target)`; the internal
compiler descriptor currently checks an explicitly supplied scalar type.

Normalization rejects duplicate table names, missing source tables or fields,
missing target tables, mismatched scalar types, and more than one reference from
the same source field. Nullable sources still require a matching scalar type.
Table and reference declaration order is preserved. Forward, self, and cyclic
references resolve without recursively normalizing tables. Acceptance of a cycle
does not promise support for any particular transactional insertion order.

The resulting `SchemaContract` provides indexed table lookup and read-only
resolved references containing the target key name, scalar type, and source
nullability. A field named `projectId` without an explicit descriptor remains an
ordinary field. A primary-key field may also be explicitly declared as a reference;
its existing nullability, default, generation, and update rules still apply.

Existing row, insert, and update validators check shape and scalar values only.
They do not look up targets. A valid non-null reference literal default also
requires a target-existence check when used by a transaction, not at schema
normalization time. Complete rows still require nullable reference fields unless
insert preparation supplies an explicit default.

### Required storage guarantees

Storage enforcement remains unimplemented. Future adapters must satisfy these
requirements before claiming support for this reference contract:

- **Atomicity:** a failed write or reference check leaves none of the command's
  row changes committed. Idempotency results and durable delivery-log entries
  remain consistent with that same transaction.
- **Consistency:** inserts and updates cannot commit dangling non-null references;
  deletes cannot leave dependents pointing at removed rows. Use native foreign-key
  constraints where applicable. Migration activation must validate existing data.
- **Isolation:** a concurrent target deletion and dependent insertion cannot both
  commit a dangling reference. A separate application-level existence read followed
  by a write is insufficient. Enforcement and locking/isolation belong to the
  authoritative transaction; constraint-check timing is later adapter work.
- **Durability:** acknowledged commits survive recovery according to the supported
  deployment's documented durability contract. Do not silently weaken durability
  for performance or claim support for guarantees an adapter cannot supply.

Local replicas can contain authorized partial data; a locally absent target is
not proof of a server-side integrity violation. Publish relevant committed changes
atomically within the architecture's subscription-group boundary.

Storage conformance tests must cover missing targets, invalid updates, referenced
deletes, nullable references, multi-row rollback, concurrent insert/delete races,
and commit recovery. These tests require storage transactions and are not provided
by the current schema-only integration suite.

Convex is a reference for validator-driven authoring and typed references.
Its documents receive generated string `_id` values; Hulla DB's chosen direction
adds configurable primary-key types. See
[Convex schemas](https://docs.convex.dev/database/schemas) and
[document IDs and relationships](https://docs.convex.dev/database/document-ids).

## Remaining decisions for subsequent chunks

1. Additional relationship delete policies, cardinality/query navigation, and
   storage constraint timing. Referential integrity and restricted deletion are
   the accepted initial behavior.
2. Storage enforcement and migration activation for named unique constraints;
   partial/expression uniqueness and alternate reference targets. The initial
   single-field and multi-field unique descriptors are implemented. Composite
   primary keys and their tuple/reference representation remain future scope.
3. Additional field types, dynamic default execution, and wire protocol envelopes
   and versions. Literal defaults, initial scalar ranges, nullable-versus-omitted
   behavior, and the initial lossless JSON record codec are implemented.
4. Stable table/field identities, broader validation diagnostics, and generated
   projection types. Explicit entity projection normalization and projected-record
   validation are now implemented; see [query coverage](query-coverage.md).

Protocol framing, code generation, and storage mappings remain later chunks.
