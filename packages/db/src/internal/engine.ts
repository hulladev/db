/** Portable contracts used by optional engine packages. No native runtime imports. */
export { transactionDatabase } from "../database.ts";
export type { WriteDatabase } from "../database.ts";
export { DatabaseError } from "../error.ts";
export { compileSchema } from "../schema.ts";
export type { SchemaDefinition, CompiledTable } from "../schema.ts";
export { schemaDescriptor } from "../schema-contract.ts";
export type { Predicate } from "../filters.ts";
export type { WriteExpression, CompiledConflict } from "../write-expression.ts";
export type { AggregateDescriptor } from "../aggregate.ts";
export { registerClientStorage } from "../client-storage.ts";
export type { ClientStorage, StoredSession, QueuedMutation, ReplicaContract, ReplicaChange } from "../client-storage.ts";
export { replicaSources } from "../database-metadata.ts";
export { schemaIdentity } from "../database-metadata.ts";
export type { DatabaseIdentity } from "../database-metadata.ts";
export type { SourcesOf } from "../connection-options.ts";
export { captureHostDatabase } from "../database-capture-host.ts";
export { registerStorageExecution, registerStorageChanges } from "../storage-execution.ts";
export type { StorageExecution, StorageChange } from "../storage-execution.ts";
