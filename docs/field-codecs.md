# Field representations and codecs

Declare the application representation beside the storage type in `db/schema.ts`.
Database operations encode writes and comparison operands, then decode returned
rows. The same schema works in direct server calls and query/mutation handlers.

## Choose an integer representation

```ts
const counters = defineTable({
  count: v.int64(),                    // number, safe integers only
  exact: v.int64({ mode: 'bigint' }),   // bigint, full signed int64 range
})
```

Number mode rejects unsafe reads and writes rather than rounding them. Bigint mode
accepts −2^63 through 2^63−1 for stored values. `v.number()` is a finite JavaScript
number stored as PostgreSQL double precision, including fractional values.

Number-mode integer sums must also fit the safe integer range. Bigint-mode sums
can exceed one column's int64 range; averages return PostgreSQL decimal strings.
Number-mode averages return finite numbers. SQL empty/null aggregate rules still
apply. Changing an application's representation does not change the SQL column.

## Distinguish instants from calendar dates

```ts
const events = defineTable({
  startsAt: v.timestamp(),
  preciseAt: v.timestamp({ mode: 'string', precision: 6 }),
  day: v.date(),
  localClock: v.timestamp({ withTimezone: false }),
})
```

| Declaration | PostgreSQL storage | Application value |
| --- | --- | --- |
| `v.timestamp()` | `timestamptz(3)` | `Date` |
| `v.timestamp({ mode: 'string' })` | `timestamptz(3)` | UTC ISO string |
| `v.timestamp({ withTimezone: false })` | `timestamp(3)` | ISO clock string, without a zone |
| `v.date()` | `date` | `YYYY-MM-DD` string |
| `v.date({ mode: 'date' })` | `date` | Midnight UTC `Date` |

Timezone-aware timestamps preserve an instant, not its original timezone name.
String inputs require an explicit offset or `Z` and normalize to UTC. Timezone-free
strings reject offsets. Their optional `mode: 'date'` maps clock components to UTC
by convention; it does not give the stored value timezone semantics.

Calendar Date mode writes the Date's UTC calendar day, discarding the clock part.
Formatting that Date in a local timezone can display another day. Use the default
calendar string when a timezone-free day is what the application needs.

Timestamp precision is 0–6, defaulting to 3. Date mode permits at most 3; use string
mode or a suitable codec for finer values. Values finer than the configured
precision reject through the SDK instead of being silently rounded. Direct SQL
casts retain PostgreSQL's own rounding rules. Supported calendar years are
0001–9999; invalid dates, infinite timestamps and leap-second spellings reject.

Built-in Date values survive generated browser calls as Dates, including nested
results and inputs. Each decode creates an independent instance. Unrestricted JSON
still rejects Dates; a column representation is separate from JSON serialization.

## Map an application value explicitly

A field codec maps your value to the base field's representation and back. For
example, server code can work with URLs while PostgreSQL stores text:

```ts
const links = defineTable({
  address: v.string().codec({
    encode: (value: URL) => value.href,
    decode: value => new URL(value),
  }),
})
```

`db.insert('links', { address: new URL('https://example.com/') })` encodes the URL;
reading it returns a URL. Encode results are checked against the underlying field.
Callbacks must be synchronous and deterministic. Null passes through without
calling either callback; omission leaves native defaults or existing values alone.

Equality, membership and range operands use application values and are encoded.
Text fragments and SQL patterns use stored strings directly:

```ts
await db.select('links')
  .where('address', '=', new URL('https://example.com/'))
  .where('address', 'contains', 'example.com')
```

Ordering and SQL expressions use storage semantics. A number decoded to a Money
class sorts numerically; text decoded to a number sorts as text. Arithmetic operands
use the stored numeric representation. Custom codecs do not run on aggregate
results; those describe SQL values whose range or meaning can differ from a row.
Built-in modes still apply, including Date extrema and safe-number checks.

Custom classes may be returned by direct server database calls. For browser
declarations, convert them in the handler or output schema to supported transport
values. A field codec does not create a shared class transport registry. Built-in
Date transport is supported explicitly; arbitrary prototypes are rejected.

## Convert JSON members explicitly

```ts
const settings = v.json({
  counter: v.string().codec({
    encode: (value: bigint) => value.toString(),
    decode: value => BigInt(value),
  }),
})
```

The application sees a bigint member and the JSON column stores a string. Without
an explicit codec, JSON rejects bigint, Date and class instances. Object/array shape
validation still rejects unknown properties and invalid values. Writes replace the
whole JSON column; codecs do not introduce nested patches.

See [server database operations](server-database.md) for projections, aggregate
results and atomic update expressions, and [JSON values](json-values.md) for shapes.
