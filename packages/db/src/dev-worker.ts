import { dirname, resolve } from "node:path";
import { generateProject } from "./project-codegen.ts";
import { startService } from "./service.ts";
import { startStudio, type StudioService } from "./studio.ts";
import type { StudioFlags } from "./cli-options.ts";
import { initializeDatabase, migrateDatabase } from "./server-database.ts";
import { DatabaseError } from "./error.ts";
import { resolveDatabaseConnection } from "./local-connection.ts";
import { cliError, cliOutput } from "./cli-output.ts";
import { loadHostConfiguration } from "./host-configuration.ts";
import { AddressConflict, addressURL, startCLIListener, StartupCancelled, type DevelopmentAddresses, type ListenAddress } from "./cli-address.ts";

let stopping = false;
let service: Awaited<ReturnType<typeof startService>> | undefined;
let studio: StudioService | undefined;
let cancelDatabaseRequest: (() => void) | undefined;
const startup = new AbortController();
const addresses = JSON.parse(process.argv[4] ?? "{}") as DevelopmentAddresses;
function remembered(kind: "service" | "studio", requested: ListenAddress): ListenAddress {
  const choice = addresses[kind];
  return choice?.requested.host === requested.host && choice.requested.port === requested.port ? choice.address : requested;
}
function remember(kind: "service" | "studio", requested: ListenAddress, address: ListenAddress) {
  process.send?.({ listening: { kind, requested, address } });
}
function developmentURL(address: ListenAddress): string {
  const host = address.host === "0.0.0.0" ? "127.0.0.1" : address.host === "::" ? "::1" : address.host;
  return addressURL({ host, port: address.port });
}
const stop = () => {
  stopping = true;
  startup.abort();
  cancelDatabaseRequest?.();
  void service?.close().catch(() => {});
  void studio?.close().catch(() => {});
};
process.on("SIGINT", stop); process.on("SIGTERM", stop);
try {
  const path = resolve(process.argv[2]!);
  await generateProject(path, config => {
    const schema = resolve(dirname(path), config.schema ?? 'db/schema.ts');
    process.send?.({ watch: [dirname(schema), ...(config.functions ? [resolve(dirname(path), config.functions)] : [])] });
    const initial = remembered("service", { host: config.service?.host ?? "127.0.0.1", port: config.service?.port ?? 3001 });
    return initial.port ? { url: developmentURL(initial) } : undefined;
  });
  const { config, schemaPath, schema: definition } = await loadHostConfiguration(path);
  startup.signal.throwIfAborted();
  process.send?.({ watch: [dirname(schemaPath), ...(config.functions ? [resolve(dirname(path), config.functions)] : [])] });
  const managed = config.database.server.url === undefined && process.env.DATABASE_URL === undefined;
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
  const initial = remembered("service", requested);
  cliOutput.success("Generated database entrypoints.");
  if (managed && !stopping) {
    const connection = resolveDatabaseConnection(config.database.server, path);
    try { await initializeDatabase(definition, connection); }
    catch (error) {
      if (!(error instanceof DatabaseError) || error.code !== "schema_mismatch" || stopping) throw error;
      const plan = await migrateDatabase(definition, connection);
      if (plan.changes.length) cliOutput.success("Applied development schema migration.");
    }
  }
  if (!stopping) {
    const started = await startCLIListener("Database service", initial,
      address => startService(path, address), { signal: startup.signal });
    service = started.listener;
    if (stopping) await service.close();
    else {
      const url = new URL(service.url);
      const actual = { host: url.hostname.replace(/^\[|\]$/g, ""), port: Number(url.port || 80) };
      if (actual.host !== initial.host || actual.port !== initial.port) {
        await generateProject(path, { url: developmentURL(actual) });
      }
      remember("service", requested, actual);
      cliOutput.listening("Database service", service.url);
      const options = JSON.parse(process.argv[3] ?? "null") as StudioFlags | null;
      if (options && !stopping) {
        const requested = { host: "127.0.0.1", port: options.port };
        const started = await startCLIListener("Hulla Studio", remembered("studio", requested),
          address => startStudio(path, { ...options, port: address.port }), { signal: startup.signal, fixedHost: true });
        studio = started.listener;
        if (stopping) await studio.close();
        else {
          remember("studio", requested, { ...started.address, port: Number(new URL(studio.url).port || 80) });
          cliOutput.listening("Hulla Studio", studio.url);
        }
      }
      await (studio ? Promise.race([service.closed, studio.closed]) : service.closed);
    }
  }
} catch (error) {
  if (error instanceof StartupCancelled) {
    cliOutput.info(error.message);
    process.send?.({ cancelStartup: true });
  } else if (error instanceof AddressConflict && process.send) {
    process.send({ startupError: error.message });
  } else if (!stopping) {
    cliError.error(error instanceof Error ? error.message : "Development startup failed");
    cliError.info("Waiting for source changes before restarting.");
    process.exitCode = 1;
  }
} finally {
  process.off("SIGINT", stop); process.off("SIGTERM", stop);
  await studio?.close().catch(() => {});
  await service?.close().catch(() => {});
  if (process.connected) process.disconnect?.();
}
