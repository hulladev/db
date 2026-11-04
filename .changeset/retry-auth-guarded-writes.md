---
"@hulla/db-better-auth": patch
---

Retry internal bulk updates and deletes after confirmed serialization rollbacks, so concurrent Better Auth guarded counters can settle without replaying application transaction callbacks.
