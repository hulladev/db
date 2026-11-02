import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createDatabase, initializeDatabase } from "../src/server-database.ts";
import { defineSchema, defineTable, v } from "../src/schema.ts";

const url = process.env.HULLADB_TEST_URL, namespace = process.env.HULLADB_TEST_NAMESPACE;
if (!url || !namespace || !process.env.HULLADB_ENGINE) throw new Error("Run through the native schema_migration integration target");
const connection = { url, namespace };
const original = defineSchema({ tasks: defineTable({ title: v.string() }) });
const upgraded = defineSchema({
  tasks: defineTable({
    priority: v.int64({ mode: "bigint" }).default(9223372036854775807n),
    title: v.string(), note: v.string().nullable(),
    settings: v.json().default({ quote: "'\\; --", values: [1, true] }),
    done: v.boolean().default(false),
  }),
  audit: defineTable({ message: v.string() }),
});
const before = createDatabase(original, connection);
const after = createDatabase(upgraded, connection);
const directory = await mkdtemp(resolve(tmpdir(), "hulla-schema-migration-"));
const config = resolve(directory, "db.config.ts"), schemaFile = resolve(directory, "schema.ts");
const runtime = process.env.HULLADB_TEST_RUNTIME ?? process.execPath;
const schemaImport = new URL("../src/schema.ts", import.meta.url).href;
const configImport = new URL("../src/config.ts", import.meta.url).href;
await Bun.write(config, `import {defineConfig} from ${JSON.stringify(configImport)}; export default defineConfig({schema:'./schema.ts',database:{server:${JSON.stringify(connection)}}});`);
await Bun.write(schemaFile, `import {defineSchema,defineTable,v} from ${JSON.stringify(schemaImport)}; export default defineSchema({tasks:defineTable({priority:v.int64({mode:'bigint'}).default(9223372036854775807n),title:v.string(),note:v.string().nullable(),settings:v.json().default({quote:"'\\\\; --",values:[1,true]}),done:v.boolean().default(false)}),audit:defineTable({message:v.string()})});`);
async function cli(...args: string[]) {
  const child = Bun.spawn([runtime, "--conditions=hulladb-source", fileURLToPath(new URL("../src/cli.ts", import.meta.url)), "migrate", config, ...args], { env: { ...process.env, NODE_ENV: "production" }, stdout: "pipe", stderr: "pipe" });
  const [stdout, stderr, code] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited]);
  return { stdout, stderr, code };
}
try {
  await initializeDatabase(original, connection);
  const saved = await before.insert("tasks", { title: "Preserve this row" });
  const inspected = await cli("--check");
  assert.equal(inspected.code, 0, inspected.stderr);
  assert.match(inspected.stdout, /Add field/);
  assert.match(inspected.stdout, /No changes applied/);
  assert.equal((inspected.stdout + inspected.stderr).includes(url), false);
  assert.equal((await before.select("tasks")).length, 1);
  await assert.rejects(Promise.resolve(after.select("tasks")), { code: "schema_mismatch" });
  const applied = await cli();
  assert.equal(applied.code, 0, applied.stderr);
  assert.match(applied.stdout, /migration applied/);
  const row = await after.select("tasks").where({ id: saved.id }).unique();
  assert.ok(row);
  assert.equal(row.title, saved.title);
  assert.equal(row.priority, 9223372036854775807n);
  assert.equal(row.note, null); assert.equal(row.done, false);
  assert.deepEqual(JSON.parse(JSON.stringify(row.settings)), { quote: "'\\; --", values: [1, true] });
  await assert.rejects(Promise.resolve(before.select("tasks")), { code: "schema_mismatch" });
  await after.insert("audit", { message: "New table works" });
  const again = await cli();
  assert.equal(again.code, 0, again.stderr); assert.match(again.stdout, /already current/);
  await Bun.write(schemaFile, `import {defineSchema,defineTable,v} from ${JSON.stringify(schemaImport)}; export default defineSchema({tasks:defineTable({title:v.boolean()})});`);
  const rejected = await cli();
  assert.notEqual(rejected.code, 0); assert.match(rejected.stderr, /unsupported schema change/);
  assert.equal((await after.select("tasks"))[0]!.id, saved.id);
} finally { await before.close(); await after.close(); await rm(directory, { recursive: true, force: true }); }
