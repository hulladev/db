#!/usr/bin/env node
import { resolve, dirname } from "node:path";
import { pathToFileURL } from "node:url";
import { parseCLI, usage } from "./cli-options.ts";
import { cliError, cliOutput } from "./cli-output.ts";

async function main() {
  const args = process.argv.slice(2);
  if (!args.length || args.includes("--help") || args.includes("-h")) { cliOutput.help(); return; }
  const { command, config, check } = parseCLI(args);
  const configPath = resolve(config);
  if (command === "generate" || command === "dev") {
    if (typeof Bun === "undefined") throw new Error(`hulla-db ${command} requires Bun. Run bun --bun hulla-db ${command}.`);
    try { await import("typescript/unstable/async"); }
    catch { throw new Error("Code generation requires TypeScript 7.0.2. Install it with bun add --dev typescript@7.0.2."); }
  }
  cliOutput.heading(command, configPath);
  if (command === "generate") {
    const { generateProject } = await import("./project-codegen.ts");
    await generateProject(configPath);
    cliOutput.success("Generated database entrypoints.");
  } else if (command === "serve" || command === "dev") {
    const service = command === "dev"
      ? await (await import("./dev.ts")).startDevelopment(configPath)
      : await (await import("./service.ts")).startService(configPath);
    const stop = () => { void service.close().catch(() => {}); };
    process.on("SIGINT", stop); process.on("SIGTERM", stop);
    if ("url" in service && typeof service.url === "string") cliOutput.listening("Database service", service.url);
    try { await service.closed; }
    finally {
      process.off("SIGINT", stop); process.off("SIGTERM", stop);
      await service.close();
    }
  } else {
    const { defineConfig } = await import("./config.ts");
    const { initializeDatabase, migrateDatabase } = await import("./server-database.ts");
    const config = defineConfig((await import(pathToFileURL(configPath).href)).default);
    const schemaPath = resolve(dirname(configPath), config.schema ?? "db/schema.ts");
    const schema = (await import(pathToFileURL(schemaPath).href)).default;
    const connection = config.database.server;
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
  if (error instanceof Error && error.message === usage) {
    cliError.error("Invalid command or options."); cliError.help(); process.exitCode = 1;
  } else {
    cliError.error(error instanceof Error ? error.message : "Database setup failed"); process.exitCode = 1;
  }
}
