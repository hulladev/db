# Projections and function-result coverage

Entity projections are schema contracts. Runtime query completeness belongs to an
exact function invocation, not an inferred local table replica.

## Projection contracts

`TableContract::projection(name, fields)` resolves an explicit nonempty field
selection against a normalized table. An entity projection must include the
selected primary key. Unknown names, repeated fields, and malformed selections
are errors. The field order is preserved, and newly added table fields do not
become part of an existing projection.

`ProjectionContract::validate_record()` requires every selected field, enforces
its scalar type and nullability, and rejects unselected fields. An omitted
nullable field is incomplete; explicit null is a value. Insert defaults are
removed from the projection and never fill missing result fields. Full-row,
insert, patch, and projected-record validation remain separate operations.

These are entity projections. Aggregate rows, computed fields, joined/nested
results, generated frontend types, and stable schema identities remain future
contracts.

## Runtime query results

The managed runtime evaluates registered queries in an authorized snapshot and
pairs their result with a source identity and scoped revision. Cache and
subscription identity includes the function, arguments and authorization context.
A complete result for one invocation does not establish coverage for a different
filter, page, function or identity. Refresh reevaluates the query so newly matching
rows can enter a bounded result.

See [managed functions](managed-functions.md) for snapshot and transaction guarantees,
[deployed adapters](sdk-adapters.md) for live result delivery and browser caching,
and [Rust remote clients](rust-remote-client.md) for durable function-result storage.
General local table replication and arbitrary local query coverage remain deferred.
