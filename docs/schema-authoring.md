# TypeScript schema authoring and compilation

Define tables once in `db/schema.ts` to share database types, constraints and
optional runtime validation schemas. `defineSchema` registers their database
names; generation supplies schema-bound query/mutation constructors and direct
server database access. Schema generation does not change storage: initialization
is an explicit operation.

## Authoring

```ts
// db/schema.ts
import { defineSchema, defineTable, v } from '@hulla/db/schema'

const publications = defineTable({
  slug: v.string(),
  title: v.string(),
}, { primaryKey: 'slug' })
const articles = defineTable({
  publication: v.id(publications),
  title: v.string().min(1).max(200),
  published: v.boolean().default(false),
  excerpt: v.string().nullable(),
  ordinal: v.int64({ mode: 'bigint' }).default(0n),
}, {
  unique: {
    publication_title: { fields: ['publication', 'title'] },
    excerpt: { fields: ['excerpt'], nulls: 'distinct' },
  },
}).index(['publication', 'published'])

export default defineSchema({ publications, articles })
```

Scalars include string, boolean, finite numbers (`v.number()`), signed int64
(`number` by default, explicit bigint mode for the full range), calendar dates,
timestamps, and UUID (a hyphenated UUID string). JSON columns support structured
objects and arrays; see [JSON values and predicates](json-values.md). UUID validators normalize case. String values must contain valid Unicode
scalars so Rust emission cannot replace unpaired UTF-16 surrogates.

Choose built-in modes or `.codec({ encode, decode })` on the field to control its
application representation. See [field representations and codecs](field-codecs.md)
for safe integers, UTC/date precision, custom classes and explicit JSON conversions.

Nullable fields still require a value on inserts and complete rows; use
`.nullable().default(null)` to allow omission during insertion. Defaults are
validated literal values. Patches allow field omission and forbid primary-key
assignment. Validation rejects unknown fields and accepts only plain records.

An omitted key declaration injects a UUID `id`. UUID keys default to UUIDv7;
`defineSchema(tables, { defaultUuidGeneration: 'v4' })` changes that fallback.
An explicitly selected string or int64 key defaults to no generator. Table options
can set `keyGeneration` to `'none'`, `'uuidV4'`, `'uuidV7'`, or
`{ handler: 'module.function', outputType: 'int64' }`. Custom handlers are metadata; the managed PostgreSQL runtime registers and executes
them within mutation transactions.
`insertKeyPolicy: 'generatedOnly'` requires a generator and forbids supplied keys.

References target the selected primary key and adopt its scalar and codec. Both
`v.id(publications)` and `v.id('publications')` resolve through the assembled schema;
strings avoid circular module initialization. The `defineSchema` object key is the
database table name, including casing or spaces. A table object must have one
unambiguous registration. Named unique constraints can contain
one or several fields. Nullable unique fields require an explicit `nulls` policy:
`'distinct'` permits repeated nulls; `'notDistinct'` compares nulls as equal.
The compiler rejects duplicate field sets, missing fields and incompatible keys,
defaults, generators and references. Storage adapters must enforce uniqueness,
target existence and referenced-row deletion policies in transactions.

## Enforce value rules and checks

String `.min(n)`/`.max(n)` measure Unicode codepoints. Numeric bounds measure stored
values, including int64 bounds without rounding. `.oneOf([...])` restricts scalar
choices. Bounds are inclusive; nullable fields still allow null independently.
These rules validate encoded writes/defaults and become PostgreSQL checks, so SQL
expression updates must also satisfy them.

```ts
const products = defineTable({
  title: v.string().min(1).max(200),
  price: v.number().min(0),
  state: v.string().oneOf(['draft', 'published']).default('draft'),
})
```

Field codecs do not bypass rules: the encoded value must satisfy storage constraints.
Comparisons and arithmetic operands only need the storage type. A nonnegative
stock column can still be filtered with `< 0` or decremented by adding `-1`; its
final stored value cannot become negative. Chained bounds tighten earlier bounds;
contradictory rules fail during authoring.

For a rule involving several columns, import `sql` from `@hulla/db/schema` and
declare a named table check:

```ts
const ranges = defineTable({ low: v.number(), high: v.number() }, {
  checks: fields => ({
    valid_range: sql`${fields.low} <= ${fields.high}`,
  }),
})
```

Template text is trusted PostgreSQL expression code. Interpolated fields are
quoted column identifiers; scalar interpolations are quoted values, not executable
SQL. Checks accept one expression, without statement separators or comments.
PostgreSQL's CHECK semantics apply: false rejects a row, while SQL null passes.
Use nullability separately when a value must exist. Arbitrary checks, uniqueness
and foreign keys cannot be converted into independent input validators.

Declarative bounds/choices currently apply to scalar columns. Constrained fields
inside JSON are rejected rather than losing rules in the native contract. Use a
function input validator for nested input rules or a named SQL check for a JSON
storage rule.

## Declare indexes and deletion policies

Chain `.index(['field', ...])` on a table definition. A deterministic name is
generated unless supplied as the second argument:

```ts
const articles = defineTable({ publication: v.id(publications), title: v.string() })
  .index(['publication', 'title'], { name: 'articles_by_publication_title' })
```

Declarations are immutable; keep the returned table. Index fields are typed and
validated. Indexes are created during explicit storage initialization. Queries
let PostgreSQL choose an index and never create one as a side effect. Advanced
index expressions, ordering and partial indexes are outside this surface.

Deletion restricts referenced rows by default. Choose a different policy on a
reference where the domain calls for it:

```ts
const notes = defineTable({ folder: v.id('folders', { onDelete: 'cascade' }) })
const bookmarks = defineTable({
  folder: v.id('folders', { onDelete: 'setNull' }).nullable(),
})
```

`setNull` requires a nullable field. Cascades and nulling happen atomically with
the parent write, affect accounting and revisions, and refresh dependent live
queries. A later failure rolls them back. A primary key can reference another
table for a one-to-one extension; it must be supplied instead of independently
generating an unrelated key.
Configure field bounds, choices and codecs on the referenced primary key. `v.id`
does not accept independent field constraints; use a named SQL check for a
reference-specific subset rule.

## Infer types and build function validators

Tables expose type-only `$inferSelect`, `$inferInsert` and `$inferUpdate` properties:

```ts
type Article = typeof articles.$inferSelect
type NewArticle = typeof articles.$inferInsert
type ArticleChanges = typeof articles.$inferUpdate
```

Generated `Row`, `Insert`, `Patch` and `Id` aliases additionally bind the complete
schema and exact table identity. Prefer them for shared helpers and references,
particularly string references to custom primary keys. ID types reject a known ID
from another table while accepting plain scalar input from validators; database
key/reference checks validate the actual value.

The optional `@hulla/db/zod` adapter uses your installed Zod 4 peer to return native,
strict, extensible objects. The core schema/runtime does not depend on Zod:

```ts
import { createInsertSchema } from '@hulla/db/zod'

const newArticle = createInsertSchema(articles, {
  title: schema => schema.max(100),
}).omit({ publication: true })
```

Use `createSelectSchema` for complete rows and `createUpdateSchema` for optional
writable fields excluding the primary key. Insert schemas make defaulted/generated
fields optional without applying database defaults, and exclude generated-only
keys. Nullable fields without a default remain required. Field refinements are
applied before nullable/optional wrappers; underlying storage rules still apply.
There is no implicit coercion or codec transformation of validator output.

For a custom codec's application type, explicitly return its native validator,
such as `address: () => z.instanceof(URL)`. Arbitrary codec functions do not expose
a validation-library AST. Register tables containing string references with
`defineSchema` before adapting them, so the adapter can resolve the real key type.

## Generation and validation

```ts
// db/generate.ts — run with Bun in the trusted build environment
import { generateSchema } from '@hulla/db/codegen'

await generateSchema({
  schema: new URL('./schema.ts', import.meta.url).pathname,
  outDir: new URL('./_generated', import.meta.url).pathname,
  // Optional: otherwise schema.rs is emitted inside outDir.
  rustOutput: new URL('../server/generated/schema.rs', import.meta.url).pathname,
})
```

Generation imports and evaluates the authored module. Keep credentials,
connections and per-request work out of schema module initialization. All three output
destinations are checked for the schema generator ownership header before any
file is written; authored files are never overwritten. Schema and function
generation can share an output directory without retiring each other's files.
There is no automatic watcher or storage mutation.

```ts
import type { Row, Insert, Patch, Id } from './_generated/types'

type Article = Row<'articles'>
type NewArticle = Insert<'articles'>
type ArticleChanges = Patch<'articles'>
type PublicationId = Id<'publications'>
```

These imports are type-only and do not load authored schema modules in a browser.
IDs retain their table identity, reference fields use the target ID type, generated
keys and defaulted insert fields are optional, and patches reject key assignment.
Types do not replace runtime validation or decode network values.

`parseRow(schema, table, unknown)` validates complete logical rows;
`prepareInsert(schema, table, unknown)` validates inputs and fills literal defaults;
`parsePatch(schema, table, unknown)` validates partial updates. They return validated
copies and leave caller records unchanged. An omitted generated key stays pending
in a prepared insert; the runtime must execute its generator and validate the
complete row before writing. Int64/UUID wire decoding is a separate boundary.

Rust consumers with a `hulladb_schema` dependency can include the generated module:

```rust,ignore
mod generated {
    include!("generated/schema.rs");
}

let contract = generated::schema()?;
```

Rust normalization remains authoritative for native consumers. The generated
module uses existing declarations and carries no release/schema version or
migration machinery. Generated Rust application structs remain subsequent work;
TypeScript server contexts are bound by the generated `_generated/functions`
constructors. Run schema generation before function discovery so this module exists
when authored declarations import it.

## Checks

`make sdk-check` generates the publishing-schema fixture, checks TypeScript types
and runs SDK tests. `cargo test -p hulladb_schema --test authored_schema --locked`
compiles that emitted Rust module and exercises its native normalization and
insert/update validation. The fixture is checked in so the Rust test also runs
without Bun. TypeScript checks verify it matches current generator output.
