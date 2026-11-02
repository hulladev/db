import { dirname, resolve } from "node:path";
import { defineConfig } from "./config.ts";
import { generateProject } from "./project-codegen.ts";
import { startService } from "./service.ts";
import { initializeDatabase, migrateDatabase } from "./server-database.ts";
import { DatabaseError } from "./error.ts";
import { resolveDatabaseConnection } from "./local-connection.ts";
import { cliError, cliOutput } from "./cli-output.ts";

let stopping = false;
let service: Awaited<ReturnType<typeof startService>> | undefined;
let cancelDatabaseRequest: (() => void) | undefined;
const startup = new AbortController();
function developmentURL(address: { host: string; port: number }): string {
  const host = address.host === "0.0.0.0" ? "127.0.0.1" : address.host === "::" ? "::1" : address.host;
  return `http://${host.includes(":") ? `[${host}]` : host}:${address.port}`;
}
const stop = () => {
  stopping = true;
  startup.abort();
  cancelDatabaseRequest?.();
  void service?.close().catch(() => {});
};
process.on("SIGINT", stop); process.on("SIGTERM", stop);
try {
  const path = resolve(process.argv[2]!);
  const config = defineConfig((await import(Bun.pathToFileURL(path).href)).default);
  const schema = resolve(dirname(path), config.schema ?? "db/schema.ts");
  process.send?.({ watch: [dirname(schema), ...(config.functions ? [resolve(dirname(path), config.functions)] : [])] });
  const managed = config.database.server.url === undefined && !process.env.DATABASE_URL;
  if (managed) {
    process.env.DATABASE_URL = await new Promise<string>((resolve, reject) => {
      const finish = (url?: string, error?: Error) => {
        process.off("message", receive); cancelDatabaseRequest = undefined;
        if (error) reject(error); else resolve(url!);
      };
      const receive = (message: unknown) => {
        if (!message || typeof message !== "object") return;
        if ("database" in message && typeof message.database === "string") finish(message.database);
        else if ("databaseError" in message && typeof message.databaseError === "string") finish(undefined, new Error(message.databaseError));
      };
      cancelDatabaseRequest = () => finish(undefined, new Error("Development stopped"));
      process.on("message", receive);
      process.send?.({ database: true });
    });
  }
  const requested = { host: config.service?.host ?? "127.0.0.1", port: config.service?.port ?? 3001 };
  const initial = requested;
  await generateProject(path, initial.port ? { url: developmentURL(initial) } : undefined);
  cliOutput.success("Generated database entrypoints.");
  if (managed && !stopping) {
    const definition = (await import(Bun.pathToFileURL(schema).href)).default;
    const connection = resolveDatabaseConnection(config.database.server, path);
    try { await initializeDatabase(definition, connection); }
    catch (error) {
      if (!(error instanceof DatabaseError) || error.code !== "schema_mismatch" || stopping) throw error;
      const plan = await migrateDatabase(definition, connection);
      if (plan.changes.length) cliOutput.success("Applied development schema migration.");
    }
  }
  if (!stopping) {
    service = await startService(path);
    if (stopping) await service.close();
    else {
      const url = new URL(service.url);
      const actual = { host: url.hostname.replace(/^\[|\]$/g, ""), port: Number(url.port || 80) };
      if (actual.host !== initial.host || actual.port !== initial.port) {
        await generateProject(path, { url: developmentURL(actual) });
      }
      cliOutput.listening("Database service", service.url);
      await service.closed;
    }
  }
} catch (error) {
  if (!stopping) {
    cliError.error(error instanceof Error ? error.message : "Development startup failed");
    cliError.info("Waiting for source changes before restarting.");
    process.exitCode = 1;
  }
} finally {
  process.off("SIGINT", stop); process.off("SIGTERM", stop);
  await service?.close().catch(() => {});
  if (process.connected) process.disconnect?.();
}
