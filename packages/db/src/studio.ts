import { createRequire } from "node:module";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import type { StudioFlags } from "./cli-options.ts";
import { resolveDatabaseConnection } from "./local-connection.ts";
import { loadHostConfiguration } from "./host-configuration.ts";

export interface StudioService { readonly url: string; readonly closed: Promise<void>; close(): Promise<void> }

/** Resolve from the application so the Studio package remains entirely optional. */
export async function startStudio(configPath: string, options: StudioFlags): Promise<StudioService> {
  const path = resolve(configPath);
  let entry: string;
  try { entry = createRequire(path).resolve("@hulla/db-studio"); }
  catch { throw new Error("Studio requires @hulla/db-studio. Install it alongside @hulla/db to use hulla-db studio or dev --studio."); }
  const { config, schema } = await loadHostConfiguration(path);
  const studio = await import(pathToFileURL(entry).href);
  return studio.startStudio({ schema, connection: resolveDatabaseConnection(config.database.server, path), limits: config.limits,
    accessToken: process.env.HULLADB_STUDIO_TOKEN, ...options });
}
