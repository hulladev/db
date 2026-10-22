# Store and query JSON values

Use JSON columns for settings, document payloads, and ordered lists that belong to
one row. Define the shape in `db/schema.ts`; generated row types and runtime writes
then use the same contract.

```ts
import { defineSchema, defineTable, v } from '@hulla/db/schema'

export default defineSchema({
  profiles: defineTable({
    score: v.number(),
    settings: v.object({
      theme: v.string().nullable().optional(),
      tags: v.array(v.string()),
      appearance: v.object({ compact: v.boolean() }),
    }),
  }),
})
```

`v.number()` stores a finite JavaScript number as PostgreSQL double precision.
Use `v.int64({ mode: 'bigint' })` for exact signed 64-bit integers represented as
`bigint`; the default `v.int64()` mode uses safe JavaScript numbers.
Neither number nor JSON columns can be primary keys or references.

**Shorthand:** `v.json({ ... })` means `v.json(v.object({ ... }))`; the outer
object validator is optional. Bare `v.object(...)` and `v.array(...)` table fields
also create validated JSON columns. For example, `tags: v.array(v.string())` stores
a JSON array. `.nullable()` permits a null column; `.optional()` applies only to
nested object members, not whole columns. Use explicit `v.json(...)` when you need
column defaults or unrestricted JSON.

## Write complete values

In server code, import `db` from your generated server module. In query and mutation
handlers, use `ctx.db` for the same operations.

```ts
import { db } from './_generated/server'

const profile = await db.insert('profiles', {
  score: 1.5,
  settings: { tags: ['editor'], appearance: { compact: true } },
})
await db.update('profiles', {
  settings: { theme: 'dark', tags: [], appearance: { compact: false } },
}).where('id', '=', profile.id)
```

The update replaces the complete `settings` value. It does not merge nested
objects or append array elements. Omitted columns and top-level patch values of
`undefined` stay unchanged. Inside JSON, `undefined`, sparse arrays, unknown
structured properties, and missing required properties are errors. An optional
property may be absent; `.nullable()` separately permits an explicit null.

JSON accepts finite numbers, strings, booleans, null, arrays, and plain objects.
Dates, bigint, class instances, accessors, and `toJSON` conversions are not
implicitly accepted. Convert application values explicitly before writing and
after reading. JSON validation is bounded to 64 levels and 100,000 visited nodes.

For payloads without a known shape, use `v.json()`. `v.json<MyType>()` adds a
TypeScript assertion but still validates only JSON compatibility at runtime;
it does not validate `MyType`. A root null requires a nullable column and maps to
SQL NULL. Nested null remains a JSON value.

## Filter nested fields and arrays

Generated table references and dotted strings select the same ordinary paths:

```ts
import { profiles } from './_generated/tables'

await db.select(profiles)
  .where(profiles.settings.theme, '=', 'dark')
  .where('settings.tags', 'contains', 'editor')

await db.select(profiles).where(({ gte, arrayLength }) =>
  gte(arrayLength(profiles.settings.tags), 2)
)
```

Array `contains` matches one complete element. For objects, every property must
match; this is not partial-object containment. `containsAny`, `containsAll`, and
`containedBy` compare membership, ignoring duplicate counts and ordering. Array
`=` and `!=` compare the complete value, including order and duplicates.
Negated forms such as `not contains` remain SQL negations: missing or null arrays
do not become matches. For an actual empty array, `containsAny []` is false,
`containsAll []` is true, and `containedBy []` is true. `arrayLength` returns null
for missing or null values.

Structured string paths use text matching operators; their `caseSensitive`
option works as for ordinary string columns. An unstructured `v.json()` path
has no declared string/array type, so ambiguous `contains` is rejected. Declare
a runtime shape when you need those comparisons. Ordering JSON columns or nested
paths is currently unsupported; select a scalar sort column instead.

Strings resolve an exact root column name first, then interpret dots as path
separators. Generated references preserve each segment, including literal dots
inside keys: `profiles.settings['display.name']` targets that exact key rather
than `settings.display.name`.

## Distinguish missing values from null

The same null rules apply to object predicates, positional comparisons, and
callback helpers. `= null` normalizes to `IS NULL`; `!= null` to `IS NOT NULL`.

| Value at the path | `is null` | `is not null` | `exists` |
| --- | --- | --- | --- |
| Missing, including a missing/null parent | true | false | false |
| Explicit nested JSON null | true | false | true |
| Present non-null value | false | true | true |

```ts
// Find an explicit JSON null, excluding an absent theme.
await db.select(profiles)
  .where('settings.theme', 'exists')
  .where('settings.theme', 'is', null)
```

`where(path, 'not exists')` and `where(path, 'exists', false)` select missing
paths. Callback filters provide `exists`, `isNull`, and `isNotNull` for composed
conditions. Never pass `undefined` as a comparison operand; it is rejected.
