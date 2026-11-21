---
"@hulla/db": minor
---

Add `defaultInsertKeyPolicy` to `defineSchema`. A table can override it with
`insertKeyPolicy`. Tables with a key generator now default to `generatedOnly`.
Set `allowExplicit` for inserts that supply their own generated primary keys.
Tables without a generator default to `allowExplicit` and require a supplied key.
Use `createInsertSchema(schema.tables.name)` to inherit the schema policy in Zod.
