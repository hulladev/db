import { expect, test } from "bun:test";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { join } from "node:path";
import { generateDatabase } from "../src/database-codegen.ts";
import { initializeDatabase } from "./fixtures/sqlite.ts";
import { executeDefinition } from "../src/declarations.ts";
import { runtimeExportConditions } from "../src/runtime-conditions.ts";
import { FunctionAnalysis } from "../src/function-analysis.ts";
import { defineSchema, defineTable, v } from "../src/schema.ts";
import type { Operation } from "../src/contracts.ts";
import { preparedSchema } from '../src/prepared-schema.ts';

const fixtureSchema = defineSchema({ drafts: defineTable({ title: v.string() }) });
type FixtureSchema = typeof fixtureSchema;

async function project(selector: string, legacy = false) {
  const directory = await mkdtemp(new URL("./.function-source-", import.meta.url).pathname);
  try {
    await mkdir(join(directory, "ops"));
    await Bun.write(join(directory, "tsconfig.json"), JSON.stringify({ extends: "../../tsconfig.json", compilerOptions: { allowJs: true, checkJs: true }, include: ["**/*"] }));
    await Bun.write(join(directory, "schema.ts"), `import { defineSchema, defineTable, v } from '@hulla/db'
export const schema = defineSchema({ drafts: defineTable({ title: v.string().min(1).codec({ encode: (value: string) => 'stored:' + value, decode: value => value.slice(7) }) }) })
export const { query, mutation } = schema
`);
    await Bun.write(join(directory, "server.ts"), `import { sqlite } from '@hulla/db-sqlite'
import { schema } from './schema'
export const primary = sqlite({ filename: 'SERVER_STORAGE_PRIVATE_MARKER.sqlite', schema, functions: ${selector} })
throw new Error('APPLICATION_STARTUP_MUST_NOT_RUN_DURING_GENERATION')
`);
    await Bun.write(join(directory, "ops", legacy ? "drafts.cjs" : "drafts.ts"), legacy
      ? `const { query } = require('../schema.ts')
exports.list = query(({ db }) => db.select('drafts'))
`
      : `import { query } from '../schema'
export const list = query(({ db }) => db.select('drafts'))
export const secret = query(() => 'SERVER_HANDLER_PRIVATE_MARKER')
`);
    await Bun.write(join(directory, "functions.ts"), `export * as drafts from './ops/drafts'
export { list as allDrafts } from './ops/drafts'
`);
    // An explicit catalogue must not discover or evaluate sibling endpoints.
    await Bun.write(join(directory, "unselected.ts"), `throw new Error('UNSELECTED_FILE_EXECUTED')
export { list } from './ops/drafts'
`);
    return directory;
  } catch (error) { await rm(directory, { recursive: true, force: true }); throw error; }
}

test("lazy namespace barrels generate executable aliases and client-safe source contracts", async () => {
  const directory = await project("() => import('./functions')");
  let close: (() => Promise<void>) | undefined, dispose: (() => void) | undefined;
  try {
    const { schema } = await import(Bun.pathToFileURL(join(directory, "schema.ts")).href) as { schema: FixtureSchema };
    // The circular authoring imports do not require storage or catalogue evaluation.
    await generateDatabase(join(directory, "server.ts"));
    await generateDatabase(join(directory, "server.ts"));
    const generated = join(directory, "_generated/server");
    const { source } = await import(Bun.pathToFileURL(join(generated, "source.ts")).href);
    expect((await preparedSchema(join(directory, 'server.ts'), 'sqlite'))?.schema).toBe(source.schema);
    await expect(preparedSchema(join(directory, 'server.ts'), 'postgres')).rejects.toThrow('requires a postgres');
    const rows = await initializeDatabase(schema, { filename: ":memory:" }); close = rows.close;
    const inserted = await rows.insert("drafts", { title: "From SQLite" });
    const definitions = await import(Bun.pathToFileURL(join(directory, "ops/drafts.ts")).href);
    const { runtime } = await import(Bun.pathToFileURL(join(generated, "runtime.ts")).href);
    dispose = runtime.configure(() => ({ call: (operation: Operation) => rows.transaction(tx => executeDefinition(definitions.list, { db: tx, identity: undefined }, operation.input)) }));
    const { drafts, functions } = await import(Bun.pathToFileURL(join(generated, "index.ts")).href);
    expect(drafts.list).toBe(functions.allDrafts);
    expect(await drafts.list()).toEqual([inserted]);
    const title = source.tables.drafts.replica().fields.title;
    expect(title.parse("Shared")).toBe("Shared");
    expect(title.encode("Shared")).toBe("stored:Shared");
    expect(title.decode("stored:Shared")).toBe("Shared");
    expect(runtime.databaseIdentity).toBe(source);
    const bundle = await Bun.build({ entrypoints: [join(generated, "index.ts")], target: "browser", conditions: runtimeExportConditions() });
    expect(bundle.success).toBe(true);
    const contents = (await Promise.all(bundle.outputs.map(output => output.text()))).join("\n");
    expect(contents).not.toContain("SERVER_HANDLER_PRIVATE_MARKER");
    expect(contents).not.toContain("SERVER_STORAGE_PRIVATE_MARKER");
    expect(contents).not.toContain("UNSELECTED_FILE_EXECUTED");
    const check = new FunctionAnalysis();
    try {
      const path = join(generated, "source.ts"); await check.open([path]);
      const compiler = await check.project(path);
      expect(await compiler.program.getSemanticDiagnostics(path)).toEqual([]);
      expect(await compiler.program.getSemanticDiagnostics(join(directory, "server.ts"))).toEqual([]);
    } finally { await check.close(); }
  } finally { dispose?.(); await close?.(); await rm(directory, { recursive: true, force: true }); }
});

test("glob catalogues support CommonJS exports and omit unmatched files", async () => {
  const directory = await project("'./ops/*.cjs'", true);
  try {
    await generateDatabase(join(directory, "server.ts"));
    const { ops } = await import(Bun.pathToFileURL(join(directory, "_generated/server/index.ts")).href);
    expect(typeof ops.drafts.list).toBe("function");
    const manifest = await Bun.file(join(directory, "_generated/server/functions.json")).json();
    expect(Object.keys(manifest.functions)).toEqual(["ops/drafts.list"]);
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test("explicit catalogues reject foreign ownership and empty globs before writing bindings", async () => {
  const directory = await project("'./ops/*.ts'");
  try {
    await Bun.write(join(directory, "ops/foreign.ts"), `import { defineSchema, defineTable, v } from '@hulla/db'
const foreign = defineSchema({ other: defineTable({ title: v.string() }) })
export const list = foreign.query(() => 'Foreign')
`);
    let failure: unknown;
    // Await compiler RPC outside Bun's promise matcher to keep its I/O progressing.
    try { await generateDatabase(join(directory, "server.ts")); } catch (error) { failure = error; }
    expect(failure).toBeInstanceOf(TypeError);
    expect((failure as Error).message).toContain("another schema");
    expect(await Bun.file(join(directory, "_generated/server/runtime.ts")).exists()).toBe(false);
    const empty = join(directory, "empty.ts");
    await Bun.write(empty, `import { schema } from './schema'
import { sqlite } from '@hulla/db-sqlite'
export default sqlite({ filename: ':memory:', schema, functions: './absent/**/*.ts' })
`);
    failure = undefined;
    try { await generateDatabase(empty); } catch (error) { failure = error; }
    expect((failure as Error).message).toContain("matched no declaration files");
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test('omitting registration never exposes or evaluates nearby application functions', async () => {
  const directory = await project('undefined');
  try {
    await generateDatabase(join(directory, 'server.ts'));
    const manifest = await Bun.file(join(directory, '_generated/server/functions.json')).json();
    expect(manifest.functions).toEqual({});
  } finally { await rm(directory, { recursive: true, force: true }); }
});
