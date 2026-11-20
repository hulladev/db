---
"@hulla/db": minor
"@hulla/db-sqlite": minor
---

Retry SQLite startup once on lock contention and allow custom drivers to classify retryable errors. Applications can call `db.retry()` after a failed storage open while keeping the same bound operations and live queries.

The Expo adapter uses Expo Crypto for UUID generation when Web Crypto is unavailable. The `randomBytes` option remains available for custom providers.
