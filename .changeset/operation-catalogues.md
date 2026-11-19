---
"@hulla/db": minor
---

Register a static operation catalogue with either database adapter. `db.functions` exposes bound queries and mutations before storage opens, including for framework observers. Generated PostgreSQL clients expose the same operation catalogue through `createClient({ url }).functions` and connect directly over HTTP.
