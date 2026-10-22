#!/usr/bin/env node
import { resolve, dirname } from "node:path";
import { pathToFileURL } from "node:url";

const [command, configArgument = "db.config.ts", ...extra] = process.argv.slice(2);
try {
  if (extra.length || !["generate", "init", "serve", "dev"].includes(command ?? "")) {
    throw new Error("Usage: hulla-db <generate|init|serve|dev> [db.config.ts]");
  }
  const configPath = resolve(configArgument);
  if (command === "generate" || command === "dev") {
    if (typeof Bun === "undefined") throw new Error(`hulla-db ${command} requires Bun. Run bun --bun hulla-db ${command}.`);
    try { await import("typescript/unstable/async"); }
    catch { throw new Error("Code generation requires TypeScript 7.0.2. Install it with bun add --dev typescript@7.0.2."); }
  }
  if (command === "generate") {
    const { generateProject } = await import("./project-codegen.ts");
    await generateProject(configPath);
    console.log("Generated database entrypoints.");
  } else if (command === "serve" || command === "dev") {
    const service = command === "dev"
      ? await (await import("./dev.ts")).startDevelopment(configPath)
      : await (await import("./service.ts")).startService(configPath);
    const stop = () => { void service.close().catch(() => {}); };
    process.on("SIGINT", stop); process.on("SIGTERM", stop);
    if ("url" in service) console.log(`Database service listening at ${service.url}`);
    try { await service.closed; }
    finally {
      process.off("SIGINT", stop); process.off("SIGTERM", stop);
      await service.close();
    }
  } else {
    const { defineConfig } = await import("./config.ts");
    const { initializeDatabase } = await import("./server-database.ts");
    const config = defineConfig((await import(pathToFileURL(configPath).href)).default);
    const schemaPath = resolve(dirname(configPath), config.schema ?? "db/schema.ts");
    const schema = (await import(pathToFileURL(schemaPath).href)).default;
    await initializeDatabase(schema, config.database.server);
    console.log("Database initialized and schema verified.");
  }
} catch (error) {
  // Do not dump imported config contents, driver errors or connection URLs.
  console.error(error instanceof Error ? error.message : "Database setup failed");
  process.exitCode = 1;
}
