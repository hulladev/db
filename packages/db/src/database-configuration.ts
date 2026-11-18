import { compileSchema, isDefinedSchema, type SchemaDefinition } from "./schema.ts";
import type { DatabaseEngine } from "./engine.ts";
import { replicaSources, schemaIdentity, type DatabaseIdentity } from "./database-metadata.ts";
import { validateFunctionSource, type FunctionSource } from "./function-source.ts";

/** Internal configuration shared by adapters and build-time discovery. Not a connection. */
export interface DatabaseConfiguration<S extends SchemaDefinition = SchemaDefinition> {
  readonly schema: S;
  readonly engine: DatabaseEngine;
  readonly functions?: FunctionSource | undefined;
  readonly identity: DatabaseIdentity<S>;
  readonly connection: unknown;
}
export type DatabaseSource<D extends SchemaDefinition = SchemaDefinition> = DatabaseIdentity<D>;
const configurations = new WeakSet<object>();

export function databaseConfiguration<S extends SchemaDefinition>(engine: DatabaseEngine, options: { readonly schema: S; readonly functions?: FunctionSource | undefined } & object): DatabaseConfiguration<S> {
  if (!options || typeof options !== "object" || Array.isArray(options) || !isDefinedSchema(options.schema)) throw new TypeError("A database requires a defineSchema declaration");
  validateFunctionSource(options.functions);
  compileSchema(options.schema);
  if (Object.values(options.schema.tables).some(table => replicaSources.has(table)) && !engine.replicaStorage) throw new TypeError(`${engine.name} does not support local replica storage`);
  const { schema, functions, ...connection } = options;
  const configuration = Object.freeze({ schema, engine, ...(functions === undefined ? {} : { functions }), identity: schemaIdentity(schema), connection });
  configurations.add(configuration);
  return configuration;
}
export function isDatabaseConfiguration(value: unknown): value is DatabaseConfiguration {
  return typeof value === "object" && value !== null && configurations.has(value);
}
