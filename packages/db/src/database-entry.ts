import { isDatabaseConfiguration, type DatabaseConfiguration } from "./database-configuration.ts";
import { instanceConfiguration } from "./database-instance.ts";
import { isDefinedSchema, type SchemaDefinition } from "./schema.ts";

/** Prepared hosting can read configuration from an instance or a tool-captured entry. */
export function databaseEntry(exports: Readonly<Record<string, unknown>>): { readonly database: DatabaseConfiguration; readonly name: string } | undefined {
  const candidates = Object.entries(exports).flatMap(([name, value]) => {
    const database = isDatabaseConfiguration(value) ? value : instanceConfiguration(value);
    return database ? [{ name, database }] : [];
  });
  const unique = [...new Set(candidates.map(value => value.database))];
  if (unique.length > 1) throw new TypeError(`Database entrypoint must export exactly one database instance; found ${unique.length}`);
  return candidates[0];
}
export function exportedDatabase(exports: Readonly<Record<string, unknown>>): DatabaseConfiguration {
  const entry = databaseEntry(exports);
  if (!entry) throw new TypeError("Database entrypoint must export exactly one database instance; found 0");
  return entry.database;
}
export function exportedSchema(exports: Readonly<Record<string, unknown>>, expectedEngine?: string): SchemaDefinition {
  const entry = databaseEntry(exports);
  if (entry && expectedEngine && entry.database.engine.name !== expectedEngine)
    throw new TypeError(`This host requires a ${expectedEngine} database; received ${entry.database.engine.name}`);
  if (entry) return entry.database.schema;
  if (isDefinedSchema(exports.default)) return exports.default;
  const schemas = [...new Set(Object.values(exports).filter(isDefinedSchema))];
  if (schemas.length !== 1) throw new TypeError("Schema entrypoint must export one defineSchema declaration");
  return schemas[0]!;
}
