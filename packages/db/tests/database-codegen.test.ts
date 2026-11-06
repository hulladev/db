import { expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { exportedDatabase, generateDatabase } from "../src/database-codegen.ts";
import { executeDefinition } from "../src/declarations.ts";
import { defineSchema, defineTable, v } from "../src/index.ts";
import { sqlite } from "@hulla/db-sqlite";
import { initializeDatabase } from "./fixtures/sqlite.ts";
import { primary } from "./fixtures/database-server.ts";
import { byWorkspace } from "./fixtures/database-functions.ts";
import { runtimeExportConditions } from "../src/runtime-conditions.ts";
import type { FunctionRuntime } from "../src/contracts.ts";
import type { QueryCallable } from "../src/callable.ts";
import { compileWorker } from "../src/worker-codegen.ts";
import { generateProject } from "../src/project-codegen.ts";
import { loadHostConfiguration } from '../src/host-configuration.ts';

test('prepared PostgreSQL hosting reads deployment settings without running eager database startup', async () => {
  const directory = await mkdtemp(new URL('./.host-codegen-', import.meta.url).pathname);
  const key = 'HULLADB_TEST_HOST_URL';
  const previous = process.env[key];
  try {
    const entry = join(directory, 'server.ts');
    await Bun.write(join(directory, 'schema.ts'), `import { defineSchema, defineTable, v } from '@hulla/db'
export const schema = defineSchema({ drafts: defineTable({ title: v.string() }) })`);
    await Bun.write(join(directory, 'settings.ts'), `export const settings = { url: process.env.${key}, namespace: 'host_test' }`);
    await Bun.write(entry, `import { postgres } from '@hulla/db-postgres'
import { schema } from './schema'
import { settings } from './settings'
export const db = postgres({ schema, ...settings })`);
    process.env[key] = 'postgres://localhost/build_only';
    await generateDatabase(entry);
    process.env[key] = 'postgres://localhost/deployment';
    const { config, schema: hosted } = await loadHostConfiguration(entry);
    expect(config.database.server).toEqual({ url: 'postgres://localhost/deployment', namespace: 'host_test' });
    expect(Object.keys(hosted.tables)).toEqual(['drafts']);
    // Hosting must not fall back to executing the application when its artifact is missing.
    await Bun.file(join(directory, '_generated/server/_host.js')).delete();
    await expect(loadHostConfiguration(entry)).rejects.toThrow('run hulla-db generate');
  } finally {
    if (previous === undefined) delete process.env[key]; else process.env[key] = previous;
    await rm(directory, { recursive: true, force: true });
  }
});

test("generated granular and namespace replica queries share callables, enforce authorization and bundle without handlers", async () => {
  const directory = await mkdtemp(new URL("./.replica-codegen-", import.meta.url).pathname);
  const db = await initializeDatabase(primary.schema, { filename: ":memory:" });
  let dispose: (() => void) | undefined;
  try {
    const serverSource = new URL("./fixtures/database-server.ts", import.meta.url).pathname;
    const functionSource = new URL("./fixtures/database-functions.ts", import.meta.url).pathname;
    await Bun.write(join(directory, "server.ts"), `export { primary } from ${JSON.stringify(serverSource)}\n`);
    await Bun.write(join(directory, "tasks.ts"), `export { byWorkspace } from ${JSON.stringify(functionSource)}\nimport { defineSchema, defineTable, v } from ${JSON.stringify(new URL("../src/index.ts", import.meta.url).pathname)}\nconst other = defineSchema({ tasks: defineTable({ title: v.string() }) })\nexport const otherOperation = other.query(() => "OTHER_DATABASE")\n`);
    await db.insert("tasks", { workspace: "alice", title: "Public", internalNotes: "PRIVATE_COLUMN" });
    const child = Bun.spawn([process.execPath, "--conditions=hulladb-source", new URL("../src/cli.ts", import.meta.url).pathname, "generate", join(directory, "server.ts")], { stdout: "pipe", stderr: "pipe", timeout: 15_000 });
    const [exit, stderr] = await Promise.all([child.exited, new Response(child.stderr).text()]);
    expect({ exit, stderr: exit === 0 ? "" : stderr }).toEqual({ exit: 0, stderr: "" });
    // Regeneration must accept its own compiled worker without overwriting authored files.
    await generateDatabase(join(directory, "server.ts"));
    const config = join(directory, "db.config.ts");
    await Bun.write(config, `export default { schema: './server.ts', database: { server: { url: 'postgres://localhost/example' } } }\n`);
    await generateProject(config);
    const outDir = join(directory, "_generated", "server");
    await expect(loadHostConfiguration(join(directory, 'server.ts'))).rejects.toThrow('requires a postgres database');
    const granular = await import(Bun.pathToFileURL(join(outDir, "tasks.ts")).href) as { byWorkspace: QueryCallable<typeof byWorkspace> };
    const grouped = await import(Bun.pathToFileURL(join(outDir, "index.ts")).href);
    expect("otherOperation" in granular).toBe(false);
    expect(grouped.tasks.byWorkspace).toBe(granular.byWorkspace);
    const runtime = await import(Bun.pathToFileURL(join(outDir, "runtime.ts")).href);
    const provider: FunctionRuntime = {
      call: operation => db.transaction(tx => executeDefinition(byWorkspace, { db: tx, identity: "alice" }, operation.input)),
    };
    dispose = runtime.runtime.configure(() => provider);
    expect(await granular.byWorkspace({ workspace: "alice" })).toEqual([expect.objectContaining({ title: "Public" })]);
    expect(JSON.stringify(await grouped.tasks.byWorkspace({ workspace: "alice" }), (_key, value) => typeof value === "bigint" ? value.toString() : value)).not.toContain("PRIVATE_COLUMN");
    await expect(Promise.resolve(granular.byWorkspace({ workspace: "bob" }))).rejects.toThrow("DATABASE_REPLICA_SERVER_GUARD");
    const bundle = await Bun.build({ entrypoints: [join(outDir, "index.ts")], target: "browser", conditions: runtimeExportConditions() });
    expect(bundle.success).toBe(true);
    const contents = (await Promise.all(bundle.outputs.map(output => output.text()))).join("\n");
    expect(contents).not.toContain("DATABASE_REPLICA_SERVER_GUARD");
    expect(contents).not.toContain("node:sqlite");
    expect(contents).not.toContain("bun:sqlite");
    const worker = join(directory, "worker.ts");
    await Bun.write(worker, `import { schema } from ${JSON.stringify(new URL("./fixtures/database-schema.ts", import.meta.url).pathname)}\nimport * as tasks from ${JSON.stringify(functionSource)}\nimport { installWorker } from '@hulla/db/internal/worker'\ninstallWorker({ schema, modules: { tasks } })\n`);
    await compileWorker(worker);
    const clientSchema = join(directory, "client-schema.ts");
    await Bun.write(clientSchema, `import { defineSchema, defineTable, v } from '@hulla/db'
const primary = defineSchema({ tasks: defineTable({ title: v.string(), private: v.string() }) })
export const device = defineSchema({ tasks: primary.tables.tasks.omit('private').replica() })
`);
    const schemaBundle = await Bun.build({ entrypoints: [clientSchema], target: "browser", conditions: runtimeExportConditions() });
    expect(schemaBundle.success).toBe(true);
  } finally {
    dispose?.(); await db.close(); await rm(directory, { recursive: true, force: true });
  }
});

test("entrypoint selection accepts aliases but refuses ambiguous database ownership", () => {
  expect(exportedDatabase({ primary, default: primary }).schema).toBe(primary.schema);
  expect(() => exportedDatabase({})).toThrow("found 0");
  expect(() => exportedDatabase({ primary, another: { ...primary } })).not.toThrow();
  const another = sqlite({ schema: defineSchema({ other: defineTable({ title: v.string() }) }), filename: ":memory:" });
  expect(() => exportedDatabase({ primary, another })).toThrow("found 2");
  void another.close();
});
