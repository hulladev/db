# Task delivery moved to managed synchronization

The former CDC-triggered task feed has been replaced by `hulladb_sync`. Managed
transactions commit business data, recovery receipts and synchronization revisions
atomically; authorized results are persisted on clients in SQLite.

See [managed synchronization](managed-sync.md) for the architecture decision,
correctness invariants, write boundary, recovery and validation. `hulladb_cdc`
remains a diagnostic capture module and is not required by task serving.
