#!/usr/bin/env node
import { resolve, dirname } from "node:path";
import { pathToFileURL } from "node:url";
import { parseCLI, usage } from "./cli-options.ts";
import { cliError, cliOutput } from "./cli-output.ts";
import { startCLIListener, StartupCancelled } from "./cli-address.ts";

async function main() {
  const args = process.argv.slice(2);
  if (!args.length || args.includes("--help") || args.includes("-h")) { cliOutput.help(); return; }
  const { command, config, studio, check } = parseCLI(args, process.env.NODE_ENV === "production");
  const configPath = resolve(config);
  if (command === "generate" || command === "dev") {
    if (typeof Bun === "undefined") return (await import("./development-runtime.ts")).runDevelopmentCLI();
  }
  cliOutput.heading(command, configPath);
  if (command === "generate") {
    const { generateProject } = await import("./project-codegen.ts");
    await generateProject(configPath);
    cliOutput.success("Generated database entrypoints.");
  } else if (command === "serve" || command === "dev" || command === "studio") {
    let service: { close(): Promise<void>; closed: Promise<void>; url?: string } | undefined;
    let stopping = false;
    const startup = new AbortController();
    const stop = () => { stopping = true; startup.abort(); void service?.close().catch(() => {}); };
    process.on("SIGINT", stop); process.on("SIGTERM", stop);
    try {
      if (command === "dev") service = await (await import("./dev.ts")).startDevelopment(configPath, studio);
      else if (command === "studio") {
        const { startStudio } = await import("./studio.ts");
        service = (await startCLIListener("Hulla Studio", { host: "127.0.0.1", port: studio!.port },
          address => startStudio(configPath, { ...studio!, port: address.port }), { signal: startup.signal, fixedHost: true })).listener;
      } else {
        const { defineConfig } = await import("./config.ts");
        const config = defineConfig((await import(pathToFileURL(configPath).href)).default);
        const { startService } = await import("./service.ts");
        service = (await startCLIListener("Database service", { host: config.service?.host ?? "127.0.0.1", port: config.service?.port ?? 3001 },
          address => startService(configPath, address), { signal: startup.signal })).listener;
      }
      if (stopping) await service.close();
      else if (service.url) cliOutput.listening(command === "studio" ? "Hulla Studio" : "Database service", service.url);
      await service.closed;
    }
    catch (error) { if (!stopping) throw error; }
    finally {
      process.off("SIGINT", stop); process.off("SIGTERM", stop);
      await service?.close();
    }
  } else {
    const { defineConfig } = await import("./config.ts");
    const { initializeDatabase, migrateDatabase } = await import("./server-database.ts");
    const { resolveDatabaseConnection } = await import("./local-connection.ts");
    const config = defineConfig((await import(pathToFileURL(configPath).href)).default);
    const schemaPath = resolve(dirname(configPath), config.schema ?? "db/schema.ts");
    const schema = (await import(pathToFileURL(schemaPath).href)).default;
    const connection = resolveDatabaseConnection(config.database.server, configPath);
    if (command === "migrate") {
      const plan = await migrateDatabase(schema, connection, { check: check ?? false });
      for (const change of plan.changes) {
        cliOutput.info("field" in change ? `Add ${change.kind === "add_field" ? "field" : "reference"} ${JSON.stringify(change.table)}.${JSON.stringify(change.field)}` : `${change.kind === "add_table" ? "Add table" : "Reorder fields in table"} ${JSON.stringify(change.table)}`);
      }
      cliOutput.success(plan.changes.length === 0 ? "Schema is already current." : check ? "Check complete. No changes applied. Run migrate to apply this plan." : "Schema migration applied.");
    } else {
      await initializeDatabase(schema, connection);
      cliOutput.success("Database initialized and schema verified.");
    }
  }
}
try { await main(); } catch (error) {
  // Do not dump imported config contents, driver errors or connection URLs.
  if (error instanceof StartupCancelled) { cliOutput.info(error.message); }
  else if (error instanceof Error && error.message === usage) {
    cliError.error("Invalid command or options.");
    cliError.help();
    process.exitCode = 1;
  } else {
    cliError.error(error instanceof Error ? error.message : "Database setup failed");
    process.exitCode = 1;
  }
}
