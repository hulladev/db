---
"@hulla/db": patch
---

Start the requested PostgreSQL transaction directly on a reused bridge connection. If startup fails, replace the connection before running application code. Warm queries save a separate database transaction.
