import { postgresFactory } from "./descriptor.ts";
export type { PostgresConnection } from "./descriptor.ts";

export const postgres = postgresFactory(async (schema, settings, initialize) => {
  const runtime = await import("@hulla/db/internal/postgres");
  const connection = runtime.resolvePostgresConnection(settings);
  if (initialize) await runtime.initializeDatabase(schema, connection);
  return runtime.createDatabase(schema, connection);
});
