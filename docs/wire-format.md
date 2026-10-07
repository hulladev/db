# Initial JSON value format

**Status:** Rust record codec implemented in `hulladb_wire`. The separate
`hulladb_protocol` and [task host](task-host.md) implement fixed task HTTP/SSE
contracts. General SDK conversion and network contracts remain future work; release
protocol compatibility is deferred until publication.

This format carries the current logical scalar values without depending on a
table schema. It is an initial internal contract, not a complete network protocol
or a stabilized public SDK API. `hulladb_schema` remains independent of Serde and
JSON; the separate wire crate depends on its logical values.

## Record and scalar representation

A record is one JSON object whose member names are field names. This represents
a complete row, insert input, or partial update; the caller chooses the appropriate
schema validator. Omission remains absence of a member, never a special value.

| Logical value | JSON representation | Decoded Rust value |
| --- | --- | --- |
| String | `"hello"` | `Value::String` |
| Boolean | `true` or `false` | `Value::Boolean` |
| Null | `null` | `Value::Null` |
| Signed 64-bit integer | `{"$int64":"9223372036854775807"}` | `Value::Int64` |
| UUID | `{"$uuid":"00112233-4455-6677-8899-aabbccddeeff"}` | `Value::Uuid` with exactly 16 bytes |

Example:

```json
{
  "id": { "$int64": "9223372036854775807" },
  "projectId": { "$uuid": "00112233-4455-6677-8899-aabbccddeeff" },
  "title": "Plan release",
  "completed": false,
  "notes": null
}
```

JSON numbers are rejected, including small exact integers. One integer format
applies to the entire signed range. JSON permits numbers beyond the range that
common binary64 implementations preserve exactly; tagged decimal strings avoid
that interoperability problem. See [RFC 8259, numbers](https://www.rfc-editor.org/rfc/rfc8259.html#section-6).

Plain strings remain strings. `"123"` does not become an integer, and a UUID-looking
string does not become a UUID. Tags only have special meaning in a field's object
value. A field literally named `$int64` is allowed by the codec and has no special
record-level meaning. Ordinary nested objects and arrays are outside the current
logical scalar contract and are rejected.

## Integer and UUID rules

An integer payload must be canonical decimal text between `-9223372036854775808`
and `9223372036854775807`, inclusive. Zero is `"0"`. A minus sign is permitted only
for negative values. Leading zeroes, `+`, `-0`, whitespace, fractional/exponent
forms, non-ASCII digits, and overflow are rejected. JSON escapes may spell the
same decoded text; canonicality applies to the decoded payload, not JSON escape
choices. The Rust decoder never converts integers through floating point.

A UUID payload must have 36 ASCII characters: hexadecimal groups of lengths
8-4-4-4-12, separated by hyphens. Uppercase and lowercase hexadecimal are accepted;
encoding emits lowercase. Braces, URN prefixes, missing hyphens, whitespace,
invalid digits, and wrong lengths are rejected. See the text representation in
[RFC 9562](https://www.rfc-editor.org/rfc/rfc9562.html#section-4).

The codec preserves all 128 UUID bits and imposes no version/variant restriction.
UUID generation policy remains separate from the representation of supplied keys.
Nil, maximal, and other bit patterns round trip through the current logical type.

Each tagged scalar object must contain exactly one member: `$int64` or `$uuid`,
with a string payload. Unknown tags, extra members, repeated tags, mixed tags,
and numeric/null/boolean/object/array payloads are rejected.

## Duplicate names and malformed input

`decode_record()` accepts exactly one complete JSON record, with optional surrounding
JSON whitespace. It rejects malformed documents, non-object roots, and trailing
content. Field names are compared after JSON escape decoding: `"title"` and
`"ti\u0074le"` are duplicates. Identical repeated values are also rejected.
Names and strings are not trimmed, case-folded, or Unicode-normalized.

The decoder first parses an ordered list of raw field entries. It retains repeated
names instead of deserializing directly into a map or generic JSON object that
could overwrite them. It then checks each field in input order and constructs the
logical map. Tagged scalar members are likewise retained until their shape has
been checked. Any failure returns an error without a partial record.

This explicitly avoids JSON's implementation-dependent handling of duplicate
object names. See [RFC 8259, objects](https://www.rfc-editor.org/rfc/rfc8259.html#section-4).

`WireError::Json` reports parser/root errors with a message, line, and column.
`WireError::Field` identifies a field and a structured reason: duplicate field,
unsupported value, invalid tag, invalid integer, or invalid UUID. JSON parsing
precedes field checks. Field checks follow input order; an earlier invalid value
can therefore be reported before a later duplicate name.

## Encoding and validation boundaries

`encode_record()` visits the logical map in lexicographic field-name order and
emits compact JSON using Serde's escaping. Integers and UUIDs use the text forms
above. The output is deterministic for an identical logical record; it is not
a general canonical-JSON format or a signature/hash protocol. Decoding accepts
different object orders, allowed escapes, whitespace, and UUID hex case.

The required identity is `decode_record(encode_record(values)) == values` for
every current logical scalar. Re-encoding an accepted input can change its JSON
spelling while preserving its logical values. UUID hex case is normalized only
for UUID text; string values remain unchanged.

Decoding does not check whether fields belong to a table, whether required fields
are present, or whether null is allowed. It applies no defaults or generators.
Use `validate_row()`, `prepare_insert()`, or `validate_update()` after decoding,
according to the operation. Complete rows still require every field; an empty
update preserves existing values, while an explicit null clears a nullable field.

Storage remains responsible for uniqueness, reference existence, atomic writes,
authorization-related transaction reads, and adapter compatibility. A logical
string containing NUL can round trip through this codec even if a future storage
adapter cannot store it; decoding is not proof of database compatibility.

Future TypeScript conversion must use an exact integer representation such as
`bigint`, validate its signed range, and serialize its decimal text. Converting
an already-rounded JavaScript `number` into a tag cannot recover lost precision.
No TypeScript codec or generated SDK is implemented in this chunk.

## Remaining protocol work

- Record context: table identity, operation kind, schema/protocol version, and
  command/subscription envelopes.
- Transport framing, invalid UTF-8 handling, and enforced message/field/string
  size limits before accepting network requests. The current API takes a complete
  Rust `&str` and allocates an intermediate record; it is not a bounded network
  streaming decoder.
- Generated SDK conversion and cross-language conformance tests.
- Query projections, arrays/objects/additional scalars, and any future binary
  codec, with explicit compatible versions.

The Rust suite covers exact expected output, full-range integer boundaries,
UUID parsing, Unicode/string preservation, duplicate/escaped field names,
malformed tags/documents, and integration with insert/update/row validation.
