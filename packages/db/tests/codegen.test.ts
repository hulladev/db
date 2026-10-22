import { expectCompilerError } from "./compiler-error.ts";
import { mkdtemp, mkdir, readFile, rm, writeFile, access, symlink } from "node:fs/promises";
import { resolve } from "node:path";
import { expect, test } from "bun:test";
import { API } from "typescript/unstable/async";
import { discoverFunctions, generateFunctions, generateSchema } from "../src/codegen.ts";
import { valueCodec } from "../src/codec.ts";
import type { ValueShape } from "../src/codec.ts";
import { executeDefinition } from "../src/declarations.ts";
import { contracts as generatedContracts, runtime as scope } from "./generated/runtime.ts";
import { runtime, queryBinding, $definitions } from "./generated/names.ts";
import * as names from "./fixtures/names.ts";
import { fieldMetadata, tableName } from "../src/fields.ts";
import { runtimeExportConditions } from "../src/runtime-conditions.ts";

const source = new URL("./fixtures/notes.ts", import.meta.url).pathname;

async function expectTypechecks(path: string) {
  const api = new API();
  try {
    const snapshot = await api.updateSnapshot({ openFiles: [path] });
    try {
      const project = await snapshot.getDefaultProjectForFile(path);
      expect(project).toBeDefined();
      expect(await project!.program.getSemanticDiagnostics(path)).toEqual([]);
    } finally { await snapshot.dispose(); }
  } finally { await api.close(); }
}

test("the browser bundle includes callable imports but excludes server handlers and secrets", async () => {
  const build = await Bun.build({
    conditions: runtimeExportConditions(),
    entrypoints: [new URL("./generated/notes.ts", import.meta.url).pathname],
    target: "browser",
  });
  expect(build.success).toBe(true);
  const output = await Promise.all(build.outputs.map((entry) => entry.text()));
  const bundle = output.join("\n");
  expect(bundle).not.toContain("SERVER_IMPLEMENTATION_MUST_NOT_SHIP");
  expect(bundle).not.toContain("node:sqlite");
  expect(bundle).not.toContain("Title required");
});

test("generation refuses authored output collisions before writing any modules", async () => {
  const directory = await mkdtemp(new URL("./codegen-", import.meta.url).pathname);
  try {
    const authored = "// application bootstrap\nexport const value = 1\n";
    await writeFile(resolve(directory, "notes.ts"), authored);
    await expect(
      generateFunctions({ modules: { notes: source }, outDir: directory }),
    ).rejects.toThrow("Refusing to overwrite");
    expect(await readFile(resolve(directory, "notes.ts"), "utf8")).toBe(authored);
    await expect(access(resolve(directory, "runtime.ts"))).rejects.toThrow();
  } finally {
    await rm(directory, { recursive: true });
  }
});

test("generation protects authored native manifests before evaluating or writing modules", async () => {
  const directory = await mkdtemp(new URL("./codegen-", import.meta.url).pathname);
  try {
    const authored = '{"application":"owned"}\n';
    await writeFile(resolve(directory, "functions.json"), authored);
    await expect(generateFunctions({ modules: { notes: source }, outDir: directory })).rejects.toThrow("Refusing to overwrite");
    expect(await readFile(resolve(directory, "functions.json"), "utf8")).toBe(authored);
    await expect(access(resolve(directory, "runtime.ts"))).rejects.toThrow();
  } finally { await rm(directory, { recursive: true }); }
});

test("application export names cannot shadow generated runtime or binding imports", async () => {
  const definitions = { "names.runtime": names.runtime, "names.queryBinding": names.queryBinding, "names.$definitions": names.$definitions };
  const dispose = scope.configure(() => ({
    call: operation => executeDefinition(definitions[operation.name as keyof typeof definitions], { identity: "alice" }, operation.input),
  }));
  try {
    expect(await Promise.all([runtime(), queryBinding(), $definitions()])).toEqual([
      "alice",
      "alice",
      "alice",
    ]);
  } finally {
    dispose();
  }
});

test("unresolved result shapes fail generation before any browser bindings are written", async () => {
  const directory = await mkdtemp(new URL("./codegen-", import.meta.url).pathname);
  const authored = resolve(directory, "unsupported.ts"), output = resolve(directory, "generated");
  try {
    await writeFile(authored, 'import { defineFunctions } from "../../src/declarations.ts";\nconst { query } = defineFunctions<{}, {}>();\nexport const unresolved = query((): unknown => "value");\n');
    const failure = await generateFunctions({ modules: { unsupported: authored }, outDir: output }).then(() => undefined, error => error);
    expect(failure).toBeInstanceOf(TypeError);
    expect(failure.message).toContain("Unsupported function value type: unknown");
    await expect(access(resolve(output, "runtime.ts"))).rejects.toThrow();
  } finally { await rm(directory, { recursive: true }); }
});

test("extension inputs preserve the submitted intersection while output codecs use the transformed type", async () => {
  const directory = await mkdtemp(new URL("./codegen-", import.meta.url).pathname);
  const authored = resolve(directory, "extended.ts"), output = resolve(directory, "generated");
  try {
    await writeFile(authored, `import { defineFunctions } from "../../src/declarations.ts";
import { standard } from "../fixtures/standard.ts";
export type TaskId = string & { readonly taskId: unique symbol };
const { query } = defineFunctions<{}, {}>();
const scoped = query.extend({
  input: standard<{ taskId: TaskId }, { taskId: TaskId }>(value => value as { taskId: TaskId }),
  handler: (_ctx, input) => ({ task: input.taskId }),
}).extend({
  input: standard<{ page: string }, { page: number }>(value => ({ page: Number((value as { page: string }).page) })),
  handler: (_ctx, input) => ({ pageNumber: input.page }),
});
export const transformed = scoped({
  input: standard<{ limit: number }, { limit: number }>(value => value as { limit: number }),
  output: standard<{ total: number }, { label: string }>(value => ({ label: String((value as { total: number }).total) })),
  handler: (ctx, input) => ({ total: ctx.pageNumber + input.limit }),
});
`);
    await generateFunctions({ modules: { extended: authored }, outDir: output });
    const manifest = JSON.parse(await readFile(resolve(output, "functions.json"), "utf8")) as { functions: Record<string, { input: ValueShape; result: ValueShape }> };
    const contract = manifest.functions["extended.transformed"]!;
    expect(contract.input).toEqual({ kind: "object", fields: { taskId: { shape: { kind: "string" }, optional: false }, page: { shape: { kind: "string" }, optional: false }, limit: { shape: { kind: "number" }, optional: false } } });
    const input = valueCodec(contract.input), result = valueCodec(contract.result);
    expect(input.encode({ taskId: "task-1", page: "2", limit: 3 })).toEqual({ taskId: "task-1", page: "2", limit: 3 });
    expect(() => input.encode({ taskId: "task-1", limit: 3 })).toThrow();
    expect(() => input.encode({ taskId: "task-1", page: 2, limit: 3 })).toThrow();
    expect(result.decode({ label: "5" })).toEqual({ label: "5" });
    expect(() => result.decode({ total: 5 })).toThrow();
    const proof = resolve(directory, "proof.ts");
    await writeFile(proof, `import { transformed } from "./generated/extended.ts";
import type { TaskId } from "./extended.ts";
const id = "task-1" as TaskId;
const output: PromiseLike<{ readonly label: string }> = transformed({ taskId: id, page: "2", limit: 3 });
void output;
// @ts-expect-error extensions require all submitted fields
transformed({ taskId: id, limit: 3 });
// @ts-expect-error input transforms do not change the submitted type
transformed({ taskId: id, page: 2, limit: 3 });
// @ts-expect-error schema brands remain on the browser input type
transformed({ taskId: "unbranded", page: "2", limit: 3 });
`);
    await expectTypechecks(authored);
    await expectTypechecks(proof);
  } finally { await rm(directory, { recursive: true }); }
});

test("unknown Standard Schema input fails generation before browser output is written", async () => {
  const directory = await mkdtemp(new URL("./codegen-", import.meta.url).pathname);
  const authored = resolve(directory, "unknown-input.ts"), output = resolve(directory, "generated");
  try {
    await writeFile(authored, `import { defineFunctions } from "../../src/declarations.ts";
import { standard } from "../fixtures/standard.ts";
const { query } = defineFunctions<{}, {}>();
export const unsupported = query({ input: standard<unknown, { title: string }>(() => ({ title: "parsed" })), handler: (_ctx, input) => input.title });
`);
    const failure = await generateFunctions({ modules: { unknownInput: authored }, outDir: output }).then(() => undefined, error => error);
    expect(failure).toBeInstanceOf(TypeError);
    expect(failure.message).toContain("Unsupported function value type: unknown");
    await expect(access(resolve(output, "runtime.ts"))).rejects.toThrow();
  } finally { await rm(directory, { recursive: true }); }
});

test("schema generation bootstraps typed declaration constructors and preserves ownership", async () => {
  const directory = await mkdtemp(new URL("./codegen-", import.meta.url).pathname);
  const output = resolve(directory, "generated"), authored = resolve(directory, "bound.ts");
  try {
    await generateSchema({ schema: new URL("./fixtures/schema.ts", import.meta.url).pathname, outDir: output });
    const constructors = await readFile(resolve(output, "functions.ts"), "utf8");
    await writeFile(authored, `import { query } from "./generated/functions.ts";
import { articles } from "./generated/tables.ts";
export const titles = query(async ctx => {
  // @ts-expect-error generated constructors bind the authored table names
  if (false) await ctx.db.select("missing_table");
  // @ts-expect-error generated constructors do not invent a trusted identity type
  const identity: string = ctx.identity;
  void identity;
  return (await ctx.db.select(articles)
    .where(({ and, gte, isNotNull }) => and(gte(articles.ordinal, articles.minimum), isNotNull("excerpt")))
    .orderBy(articles.ordinal, "desc")).map(row => row.title);
});

`);
    await generateFunctions({ modules: { bound: authored }, outDir: output });
    expect(await readFile(resolve(output, "functions.ts"), "utf8")).toBe(constructors);
    await expectTypechecks(authored);
    const owned = "// handwritten declaration helpers\n";
    await writeFile(resolve(output, "functions.ts"), owned);
    await rm(resolve(output, "types.ts"));
    await expect(generateSchema({ schema: new URL("./fixtures/schema.ts", import.meta.url).pathname, outDir: output })).rejects.toThrow("Refusing to overwrite");
    await expect(access(resolve(output, "types.ts"))).rejects.toThrow();
    expect(await readFile(resolve(output, "functions.ts"), "utf8")).toBe(owned);
  } finally { await rm(directory, { recursive: true }); }
});

test("ordinary exported helpers with type metadata are not discovered as declarations", async () => {
  const directory = await mkdtemp(new URL("./codegen-", import.meta.url).pathname);
  const authored = resolve(directory, "helpers.ts"), output = resolve(directory, "generated");
  try {
    await writeFile(authored, `import { defineFunctions } from "../../src/declarations.ts";
const { query } = defineFunctions<{}, {}>();
export const hello = query(() => "hello");
export const helper = { $types: { input: null as unknown, output: null as unknown }, internalCache: new Map<string, unknown>() };
export const ordinary = Object.assign(() => "private helper", { $types: { input: null as unknown, output: null as unknown } });
`);
    await generateFunctions({ modules: { helpers: authored }, outDir: output });
    const manifest = JSON.parse(await readFile(resolve(output, "functions.json"), "utf8"));
    expect(Object.keys(manifest.functions)).toEqual(["helpers.hello"]);
    const generated = await readFile(resolve(output, "helpers.ts"), "utf8");
    expect(generated).not.toContain("$definitions.helper");
    expect(generated).not.toContain("$definitions.ordinary");
  } finally { await rm(directory, { recursive: true }); }
});

test("generated table references preserve unusual names without loading the authored schema", async () => {
  const directory = await mkdtemp(new URL("./codegen-", import.meta.url).pathname);
  const authored = resolve(directory, "schema.ts"), output = resolve(directory, "generated");
  try {
    await writeFile(authored, `import { defineSchema, defineTable, v } from '@hulla/db/schema';
export default defineSchema({
  ['__proto__']: defineTable({ ['__proto__']: v.string() }),
  ['Order History']: defineTable({ title: v.string(), ['data.value']: v.json({ ['nested.value']: v.string() }) }),
  $tables: defineTable({ title: v.string() }),
});`);
    await generateSchema({ schema: authored, outDir: output });
    // References must remain usable independently of the schema's server code.
    await rm(authored);
    const generated = await import(resolve(output, "tables.ts"));
    for (const name of ["__proto__", "Order History", "$tables"]) {
      expect(tableName(generated.default[name])).toBe(name);
    }
    expect(fieldMetadata(generated.__proto__.__proto__)).toEqual({ table: "__proto__", name: "__proto__" });
    expect(generated.default["Order History"]["data.value"]["nested.value"]).toBeDefined();
    expect(fieldMetadata(generated.$tables.title)).toEqual({ table: "$tables", name: "title" });
  } finally { await rm(directory, { recursive: true }); }
});

test("reserved generated module names are rejected before evaluating source or writing files", async () => {
  const directory = await mkdtemp(new URL("./codegen-", import.meta.url).pathname);
  const authored = resolve(directory, "must-not-execute.ts"), output = resolve(directory, "generated");
  try {
    await writeFile(authored, 'throw new Error("Source was evaluated before collision validation");\n');
    for (const module of ["functions", "types", "tables", "runtime"]) {
      const failure = await generateFunctions({ modules: { [module]: authored }, outDir: output }).then(() => undefined, error => error);
      expect(failure.message).toMatch(module === "runtime" ? /Invalid function module name: runtime/ : /Function module name conflicts with a generated schema output/);
    }
    await expect(access(output)).rejects.toThrow();
  } finally { await rm(directory, { recursive: true }); }
});

test("generator cleanup preserves schema-owned modules and cross-generator destinations are protected", async () => {
  const directory = await mkdtemp(new URL("./codegen-", import.meta.url).pathname);
  const schemaSource = new URL("./fixtures/schema.ts", import.meta.url).pathname;
  try {
    await generateSchema({ schema: schemaSource, outDir: directory });
    const schemaTypes = await readFile(resolve(directory, "types.ts"), "utf8");
    const constructors = await readFile(resolve(directory, "functions.ts"), "utf8");
    await generateFunctions({ modules: { oldNotes: source }, outDir: directory });
    await writeFile(resolve(directory, "application.ts"), "// user owned\n");
    await generateFunctions({ modules: { notes: source }, outDir: directory });
    await expect(access(resolve(directory, "oldNotes.ts"))).rejects.toThrow();
    expect(await readFile(resolve(directory, "application.ts"), "utf8")).toBe("// user owned\n");
    expect(await readFile(resolve(directory, "types.ts"), "utf8")).toBe(schemaTypes);
    expect(await readFile(resolve(directory, "functions.ts"), "utf8")).toBe(constructors);
    const runtime = await readFile(resolve(directory, "runtime.ts"), "utf8");
    await rm(resolve(directory, "types.ts"));
    await expect(generateSchema({ schema: schemaSource, outDir: directory, rustOutput: resolve(directory, "runtime.ts") })).rejects.toThrow("Schema output conflicts with a generated function file");
    await expect(access(resolve(directory, "types.ts"))).rejects.toThrow();
    expect(await readFile(resolve(directory, "runtime.ts"), "utf8")).toBe(runtime);
    await expect(generateSchema({ schema: schemaSource, outDir: directory, rustOutput: resolve(directory, "functions.ts") })).rejects.toThrow("Schema outputs must use distinct paths");
    await expect(access(resolve(directory, "types.ts"))).rejects.toThrow();
  } finally { await rm(directory, { recursive: true }); }
});

test("discovery scans beside the schema without executing helpers and preserves nested identities", async () => {
  const directory = await mkdtemp(new URL("./codegen-", import.meta.url).pathname);
  const output = resolve(directory, "_generated");
  const schema = resolve(directory, "schema.ts");
  const server = new URL("../src/declarations.ts", import.meta.url).pathname;
  const declaration = `import { defineFunctions } from ${JSON.stringify(server)};
const { query } = defineFunctions<{}, {}>();
const extended = query.extend(() => ({ tenant: "team" }));
export const list = extended(ctx => ctx.tenant);
export const helper = () => "ordinary helper";
`;
  try {
    await mkdir(resolve(directory, "project-tasks"), { recursive: true });
    await mkdir(resolve(directory, "tests"), { recursive: true });
    await mkdir(output, { recursive: true });
    await writeFile(schema, 'throw new Error("schema must not execute during discovery"); export {};');
    await writeFile(resolve(directory, "helpers.ts"), 'throw new Error("helpers must not execute"); export const misleading = {kind:"query", $types: {input: 1}};');
    await writeFile(resolve(directory, "queries.ts"), declaration);
    await writeFile(resolve(directory, "project-tasks/queries.ts"), `throw new Error("declaration must not execute during discovery");\n${declaration}`);
    await writeFile(resolve(directory, "types.ts"), 'export type { list } from "./queries.ts";');
    await writeFile(resolve(directory, "queries.test.ts"), declaration);
    await writeFile(resolve(directory, "hulla.config.ts"), declaration);
    await writeFile(resolve(directory, "tests/queries.ts"), declaration);
    await writeFile(resolve(output, "ignored.ts"), declaration);
    const modules = await discoverFunctions({ schema, outDir: output });
    expect(Object.keys(modules).sort()).toEqual(["project-tasks/queries", "queries"]);
    await writeFile(resolve(directory, "project-tasks/queries.ts"), declaration);
    await generateFunctions({ schema, outDir: output });
    const nested = await readFile(resolve(output, "project-tasks/queries.ts"), "utf8");
    const generated = await import(resolve(output, "project-tasks/queries.ts"));
    const bindings = await import(resolve(output, "runtime.ts"));
    const declared = await import(resolve(directory, "project-tasks/queries.ts"));
    const observed: string[] = [];
    const release = bindings.runtime.configure(() => ({
      call: (operation: import("../src/contracts.ts").Operation) => {
        observed.push(operation.name);
        return executeDefinition(declared.list, {}, operation.input);
      },
    }));
    try {
      expect(await generated.list()).toBe("team");
      expect(observed).toEqual(["project-tasks/queries.list"]);
    } finally { release(); }
    await writeFile(resolve(output, "project-tasks/authored.ts"), "// keep application file");
    // An incomplete edit must not silently look like a removed declaration.
    await writeFile(resolve(directory, "project-tasks/queries.ts"), "export const =");
    await expectCompilerError(generateFunctions({ schema, outDir: output }), "Invalid function source");
    expect(await readFile(resolve(output, "project-tasks/queries.ts"), "utf8")).toBe(nested);
    await rm(resolve(directory, "project-tasks/queries.ts"));
    await generateFunctions({ schema, outDir: output });
    await expect(access(resolve(output, "project-tasks/queries.ts"))).rejects.toThrow();
    expect(await readFile(resolve(output, "project-tasks/authored.ts"), "utf8")).toBe("// keep application file");
  } finally { await rm(directory, { recursive: true }); }
}, 60_000);

test("discovery override restricts endpoints and rejects ambiguous extension identities", async () => {
  const directory = await mkdtemp(new URL("./codegen-", import.meta.url).pathname);
  const server = new URL("../src/declarations.ts", import.meta.url).pathname;
  const declaration = `import {defineFunctions} from ${JSON.stringify(server)};
export const value = defineFunctions<{}, {}>().query(() => 1);`;
  try {
    await mkdir(resolve(directory, "selected"));
    await writeFile(resolve(directory, "outside.ts"), declaration);
    await writeFile(resolve(directory, "selected/inside.ts"), declaration);
    const options = { schema: resolve(directory, "schema.ts"), functions: resolve(directory, "selected"), outDir: resolve(directory, "generated") };
    expect(Object.keys(await discoverFunctions(options))).toEqual(["inside"]);
    await writeFile(resolve(directory, "selected/inside.tsx"), declaration);
    await expectCompilerError(discoverFunctions(options), "Duplicate discovered function module: inside");
  } finally { await rm(directory, { recursive: true }); }
}, 60_000);

test("nested output collisions are rejected before generation overwrites any file", async () => {
  const directory = await mkdtemp(new URL("./codegen-", import.meta.url).pathname);
  try {
    await mkdir(resolve(directory, "projects"));
    await writeFile(resolve(directory, "projects/notes.ts"), "// authored");
    await expect(generateFunctions({ modules: { "projects/notes": source }, outDir: directory })).rejects.toThrow("Refusing to overwrite");
    await expect(access(resolve(directory, "runtime.ts"))).rejects.toThrow();
    await rm(resolve(directory, "projects"), { recursive: true });
    await writeFile(resolve(directory, "projects"), "// authored parent path");
    await expect(generateFunctions({ modules: { "projects/notes": source }, outDir: directory })).rejects.toThrow("Refusing to overwrite an authored path");
    await rm(resolve(directory, "projects"));
    await mkdir(resolve(directory, "elsewhere"));
    await symlink(resolve(directory, "elsewhere"), resolve(directory, "projects"));
    await expect(generateFunctions({ modules: { "projects/notes": source }, outDir: directory })).rejects.toThrow("Refusing to overwrite an authored path");
    await expect(access(resolve(directory, "elsewhere/notes.ts"))).rejects.toThrow();
    await expect(access(resolve(directory, "runtime.ts"))).rejects.toThrow();
    for (const module of ["../notes", "/notes", "projects//notes", "projects/../notes", "projects\\notes"]) {
      await expect(generateFunctions({ modules: { [module]: source }, outDir: directory })).rejects.toThrow("Invalid function module name");
    }
  } finally { await rm(directory, { recursive: true }); }
});

test("pagination eligibility follows the canonical page contract and excludes transformed plain results", async () => {
  const directory = await mkdtemp(new URL("./codegen-", import.meta.url).pathname);
  const authored = resolve(directory, "pages.ts"), output = resolve(directory, "generated");
  try {
    await Bun.write(authored, `import { query } from "../generated/deployed/functions.ts";
import { standard } from "../fixtures/standard.ts";
import type { Page } from "../../src/page.ts";
import type { Row } from "../generated/deployed/types.ts";
type Entry = Row<"entries">;
export const direct = query(async ctx => await ctx.db.select("entries", ["title"]).paginate({ limit: 2 }));
export const conditional = query({ input: standard<{ includeOwner: boolean }>(value => value as { includeOwner: boolean }), handler: (ctx, input) => ctx.db.select("entries", { title: true, owner: input.includeOwner }) });
export const plain = query(async ctx => { const page = await ctx.db.select("entries").paginate({ limit: 2 }); return { page: page.page, cursor: page.cursor }; });
export const transformed = query({ output: standard<Page<Entry>, { page: Entry[]; cursor: string | null }>(value => { const result = value as Page<Entry>; return { page: result.page, cursor: result.cursor }; }), handler: async ctx => await ctx.db.select("entries").paginate({ limit: 2 }) });
`);
    await generateFunctions({ modules: { pages: authored }, outDir: output });
    const manifest = JSON.parse(await Bun.file(resolve(output, "functions.json")).text());
    expect(manifest.functions["pages.direct"].paginated).toBe(true);
    expect(manifest.functions["pages.plain"].paginated).toBeUndefined();
    expect(manifest.functions["pages.transformed"].paginated).toBeUndefined();
    const codec = valueCodec(manifest.functions["pages.direct"].result);
    expect(codec.decode({ page: [{ title: "Visible" }], cursor: null })).toEqual({ page: [{ title: "Visible" }], cursor: null });
    expect(() => codec.decode({ page: [{ title: "Visible", owner: "Hidden" }], cursor: null })).toThrow();
    const conditionalCodec = valueCodec(manifest.functions["pages.conditional"].result);
    expect(conditionalCodec.decode([{ title: "Visible" }])).toEqual([{ title: "Visible" }]);
    expect(conditionalCodec.decode([{ title: "Visible", owner: "Included" }])).toEqual([{ title: "Visible", owner: "Included" }]);
    expect(() => conditionalCodec.decode([{ owner: "Missing title" }])).toThrow();
    const proof = resolve(directory, "proof.ts");
    await Bun.write(proof, `import { direct, plain, transformed, conditional } from "./generated/pages.ts";
direct.paginate(() => undefined);
const page = await direct();
const title: string = page.page[0]!.title;
void title;
// @ts-expect-error unselected row properties are not exposed
page.page[0]!.owner;
const row = (await conditional({ includeOwner: true }))[0]!;
const owner: string | undefined = row.owner;
void owner;
// @ts-expect-error a runtime inclusion flag does not guarantee a property
const required: string = row.owner;
void required;
// @ts-expect-error a plain page has no live range eligibility
plain.paginate(() => undefined);
// @ts-expect-error output transforms that erase the brand erase eligibility
transformed.paginate(() => undefined);
`);
    await expectTypechecks(proof);
  } finally { await rm(directory, { recursive: true }); }
}, 60_000);

test("generation rejects canonical page results on mutations", async () => {
  const directory = await mkdtemp(new URL("./codegen-", import.meta.url).pathname);
  try {
    const source = resolve(directory, "mutation.ts");
    await Bun.write(source, `import { defineFunctions } from "../../src/declarations.ts";
import type { Page } from "../../src/page.ts";
const { mutation } = defineFunctions<{}, {}>();
declare const result: Page<string>;
export const invalid = mutation(() => result);
`);
    await expectCompilerError(generateFunctions({ modules: { pages: source }, outDir: resolve(directory, "generated") }), "Paginated results require a query");
  } finally { await rm(directory, { recursive: true }); }
}, 60_000);

test("generation rejects unrelated symbol properties instead of treating them as page brands", async () => {
  const directory = await mkdtemp(new URL("./codegen-", import.meta.url).pathname);
  try {
    const source = resolve(directory, "symbol.ts");
    await Bun.write(source, `import { defineFunctions } from "../../src/declarations.ts";
const { query } = defineFunctions<{}, {}>();
declare const other: unique symbol;
export const invalid = query(() => ({ page: [] as string[], cursor: null, [other]: true }));
`);
    await expectCompilerError(generateFunctions({ modules: { pages: source }, outDir: resolve(directory, "generated") }), "Symbol properties are not portable values");
  } finally { await rm(directory, { recursive: true }); }
}, 60_000);

test("generated JSON boundaries accept recursive values and reject lossy payloads", () => {
  const definition = generatedContracts["typescript.echoJson"];
  const payload = { nested: [null, { number: 1e30, boolean: true }] };
  expect(definition.input.encode({ payload })).toEqual({ payload });
  expect(definition.result.decode(payload)).toEqual(payload);
  expect(() => definition.result.encode({ nested: undefined })).toThrow();
  expect(() => definition.result.decode({ nested: Infinity })).toThrow();
});
