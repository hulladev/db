import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { DatabaseConnection, DatabaseSettings } from "./config.ts";

/** Imported by generated Node/Bun applications; file access must stay portable. */
export function resolveDatabaseConnection(connection: DatabaseSettings, configPath: string | URL): DatabaseConnection {
  let url = connection.url ?? process.env.DATABASE_URL;
  if (!url) {
    if (process.env.NODE_ENV === "production") throw new Error("Production requires database.server.url or DATABASE_URL.");
    const path = typeof configPath === "string" ? configPath : fileURLToPath(configPath);
    try { url = JSON.parse(readFileSync(resolve(dirname(path), ".hulla/local.json"), "utf8")).url; }
    catch { throw new Error("No database connection. Run hulla-db dev for local development, or configure database.server.url / DATABASE_URL."); }
  }
  // Validate persisted state and environment input just like authored settings.
  try {
    const parsed = new URL(url!);
    if (!["postgres:", "postgresql:"].includes(parsed.protocol)) throw new Error();
  } catch { throw new Error("Invalid database connection. Configure a PostgreSQL URL or recreate local development storage explicitly."); }
  return { url: url!, ...(connection.namespace === undefined ? {} : { namespace: connection.namespace }) };
}
