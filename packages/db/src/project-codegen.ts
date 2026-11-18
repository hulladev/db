import { lstat } from "node:fs/promises";
import { dirname, relative, resolve, sep } from "node:path";
import { defineConfig, type DatabaseConfig, type ClientConnection } from "./config.ts";
import { discoverFunctions, generateFunctions } from "./codegen.ts";
import { checkGeneratedDirectory, generateSchema } from "./schema-codegen.ts";
import { compileWorker, workerBundleHeader } from "./worker-codegen.ts";
import { generateDatabase } from "./database-codegen.ts";
import { FunctionAnalysis } from "./function-analysis.ts";
import { databaseEntry } from "./database-entry.ts";
import { captureDatabaseEntry } from "./database-capture.ts";

const header = "// Generated server by @hulla/db. Do not edit.\n";
const workerHeader = "// Generated worker by @hulla/db. Do not edit.\n";
function importPath(directory: string, path: string): string {
  const value = relative(directory, path).split(sep).join("/");
  return value.startsWith(".") ? value : `./${value}`;
}

/** Generate a project from its trusted config without opening a database or listener. */
export async function generateProject(configPath = "db.config.ts", developmentClient?: ClientConnection | ((config: DatabaseConfig) => ClientConnection | undefined)): Promise<void> {
  const source = resolve(configPath);
  const inspection = new FunctionAnalysis();
  try {
    await inspection.open([source]);
    if ((await inspection.databaseDeclarations(source)).length) return await generateDatabase(source);
  } finally { await inspection.close(); }
  const exports: Record<string, unknown> = await import(Bun.pathToFileURL(source).href);
  const config = defineConfig(exports.default as DatabaseConfig);
  const fallbackClient = typeof developmentClient === 'function' ? developmentClient(config) : developmentClient;
  const clientConnection = config.database.client ?? fallbackClient;
  const root = dirname(source);
  const schema = resolve(root, config.schema ?? "db/schema.ts");
  const schemaAnalysis = new FunctionAnalysis();
  let configured = false;
  try {
    await schemaAnalysis.open([schema]);
    const captured = await captureDatabaseEntry(schema, schemaAnalysis);
    configured = !!captured && !!databaseEntry(captured);
  } finally { await schemaAnalysis.close(); }
  if (configured) return generateDatabase(schema, {
    ...(config.functions === undefined ? {} : { functions: resolve(root, config.functions) }),
    moduleRoot: resolve(root, config.moduleRoot ?? config.functions ?? dirname(schema)),
    ...(config.limits === undefined ? {} : { limits: config.limits }),
  });
  const outDir = resolve(dirname(schema), "_generated");
  const client = resolve(outDir, "client");
  await checkGeneratedDirectory(outDir);
  await checkGeneratedDirectory(client);
  const server = resolve(outDir, "server.ts");
  const worker = resolve(outDir, "worker.ts");
  const bundle = resolve(outDir, "worker.js");
  for (const [path, generatedHeader] of [[server, header], [worker, workerHeader], [bundle, workerBundleHeader]] as const) {
    const info = await lstat(path).catch((error: NodeJS.ErrnoException) => {
      if (error.code === "ENOENT") return undefined;
      throw error;
    });
    if (info && (!info.isFile() || info.isSymbolicLink() || !(await Bun.file(path).text()).startsWith(generatedHeader)))
      throw new Error(`Refusing to overwrite an authored file: ${path}`);
  }
  await generateSchema({ schema, outDir, config: source });
  const analysis = new FunctionAnalysis();
  try {
    const modules = await discoverFunctions({ schema, outDir: client,
      ...(config.functions === undefined ? {} : { functions: resolve(root, config.functions) }),
      moduleRoot: resolve(root, config.moduleRoot ?? config.functions ?? dirname(schema)),
    }, analysis);
    const entries = Object.entries(modules);
    await Bun.write(worker, `${workerHeader}\nimport schema from ${JSON.stringify(importPath(outDir, schema))}
import { installWorker } from '@hulla/db/internal/worker'
${entries.map(([, path], index) => `import * as module${index} from ${JSON.stringify(importPath(outDir, path))}`).join("\n")}

installWorker({ schema, modules: {
${entries.map(([name], index) => `  ${JSON.stringify(name)}: module${index},`).join("\n")}
} })
`);
    // Diagnose statically resolved unsupported imports before generation evaluates
    // selected declaration modules in its trusted build process.
    const compiled = await compileWorker(worker);
    await generateFunctions({ schema, outDir: client, modules, ...(clientConnection === undefined ? {} : { client: clientConnection }), ...(config.limits === undefined ? {} : { limits: config.limits }) }, analysis);
    await Bun.write(bundle, compiled);
    await Bun.write(server, `${header}\nimport config from ${JSON.stringify(importPath(outDir, source))}
import schema from ${JSON.stringify(importPath(outDir, schema))}
import type { Schema } from './types'
import { createDatabase, resolveDatabaseConnection, type Database } from '@hulla/db/server'

export const db: Database<Schema> = createDatabase(schema as Schema, resolveDatabaseConnection(config.database.server, new URL(${JSON.stringify(importPath(outDir, source))}, import.meta.url)), config.limits)
`);
  } finally { await analysis.close(); }
}
