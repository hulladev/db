import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { DatabaseConnection, DatabaseSettings } from "./config.ts";

/** Explicit settings win over the environment, including an invalid/empty URL. */
export function resolvePostgresConnection(connection: { readonly url?: string | undefined; readonly namespace?: string }): DatabaseConnection {
  const url = connection.url === undefined ? process.env.DATABASE_URL : connection.url;
  if (url === undefined) throw new Error("No PostgreSQL connection. Run hulla-db dev for managed development, or supply url / DATABASE_URL.");
  try {
    if (typeof url !== "string" || !url.trim()) throw new Error();
    const parsed = new URL(url);
    if (!["postgres:", "postgresql:"].includes(parsed.protocol)) throw new Error();
  } catch { throw new TypeError("Invalid PostgreSQL connection URL"); }
  return { url, ...(connection.namespace === undefined ? {} : { namespace: connection.namespace }) };
}

/** Imported by generated Node/Bun applications; file access must stay portable. */
export function resolveDatabaseConnection(connection: DatabaseSettings, configPath: string | URL): DatabaseConnection {
  let url = connection.url ?? process.env.DATABASE_URL;
  if (url === undefined) {
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
