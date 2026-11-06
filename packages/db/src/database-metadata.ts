import type { SchemaDefinition, TableShape, CompiledTable } from "./schema.ts";

/** Logical ownership belongs to a schema, independently of its runtime instances. */
declare const definitionType: unique symbol;
export interface DatabaseIdentity<S extends SchemaDefinition = SchemaDefinition> {
  readonly [definitionType]: true;
  readonly schema: S;
  readonly tables: S["tables"];
}
const identities = new WeakMap<SchemaDefinition, DatabaseIdentity>();
export function schemaIdentity<S extends SchemaDefinition>(schema: S): DatabaseIdentity<S> {
  let identity = identities.get(schema);
  if (!identity) identities.set(schema, identity = Object.freeze({ schema, get tables() { return schema.tables; } }) as unknown as DatabaseIdentity);
  return identity as DatabaseIdentity<S>;
}
export function isDatabaseSource(value: unknown): value is DatabaseIdentity {
  return typeof value === "object" && value !== null && "schema" in value
    && identities.get((value as DatabaseIdentity).schema) === value;
}
export interface ReplicaSource {
  readonly database: DatabaseIdentity;
  readonly name: string;
  readonly definition: TableShape;
  readonly bound: TableShape;
  readonly columns: readonly string[];
  readonly table: CompiledTable;
}

export const tableBindings = new WeakMap<object, ReplicaSource>();
export const replicaSources = new WeakMap<TableShape, ReplicaSource>();
export const functionDatabases = new WeakMap<object, DatabaseIdentity>();
