import { expectCompilerError } from "./compiler-error.ts";
import { expect, spyOn, test } from "bun:test";
import { mkdtemp, mkdir, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { defineConfig } from "../src/config.ts";
import { generateProject } from "../src/codegen.ts";
import { runtimeExportConditions } from "../src/runtime-conditions.ts";
import { API } from "typescript/unstable/async";

const url = "postgresql://app:private-password@127.0.0.1:1/app";

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

test("configuration snapshots validated connection settings without leaking secrets on errors", () => {
  const authored = { database: { server: { url, namespace: "my_app" } }, schema: "data/schema.ts" };
  const config = defineConfig(authored);
  authored.database.server.url = "postgresql://changed/app";
  expect(config.database.server.url).toBe(url);
  expect(Object.isFrozen(config.database)).toBe(true);
  expect(() => defineConfig({ database: { server: { url: "https://secret-password@example.com" } } })).toThrow("PostgreSQL connection URL");
  expect(() => defineConfig({ database: { server: { url, namespace: "a;DROP TABLE users" } } })).toThrow("namespace");
  expect(() => defineConfig({ database: { server: { url } }, browser: true } as never)).toThrow("Invalid database configuration");
  expect(() => defineConfig({ database: { server: { url } }, functions: " " })).toThrow("nonempty path");
  for (const client of [{ url: "https://secret:password@example.com" }, { url: "//untrusted.test" }, { url: "/api", headers: { authorization: "secret" } }, { url: "/api", queryTimeoutMs: 0 }])
    expect(() => defineConfig({ database: { server: { url }, client } })).toThrow();
  expect(defineConfig({ database: { server: { url }, client: { url: "/db", credentials: "include" } } }).database.client.url).toBe("/db");
  const limits = { inputBytes: 32, executionMs: 10000 };
  const bounded = defineConfig({ database: { server: { url } }, limits });
  limits.inputBytes = 99;
  expect(bounded.limits.inputBytes).toBe(32);
  for (const value of [0, -1, 1.1, Infinity, 64 * 1024 * 1024 + 1])
    expect(() => defineConfig({ database: { server: { url } }, limits: { inputBytes: value } })).toThrow("limits.inputBytes");
  expect(() => defineConfig({ database: { server: { url } }, limits: { unknown: 1 } } as never)).toThrow("limits");
});

async function project(schemaDirectory = "db", functionsDirectory = schemaDirectory) {
  const root = await mkdtemp(new URL("./config-project-", import.meta.url).pathname);
  const config = resolve(root, "db.config.ts");
  const schema = resolve(root, schemaDirectory, "schema.ts");
  await mkdir(resolve(root, functionsDirectory), { recursive: true });
  await mkdir(resolve(root, schemaDirectory), { recursive: true });
  await writeFile(config, `import { defineConfig } from '@hulla/db/config'\nexport default defineConfig(${JSON.stringify({ database: { server: { url }, client: { url: "http://localhost:9000", credentials: "include" } }, ...(schemaDirectory === "db" ? {} : { schema: `${schemaDirectory}/schema.ts` }), ...(functionsDirectory === schemaDirectory ? {} : { functions: functionsDirectory }) })})\n`);
  await writeFile(schema, `import { defineSchema, defineTable, v } from '@hulla/db/schema'\nexport default defineSchema({ tasks: defineTable({ title: v.string() }) })\n`);
  const functionsImport = functionsDirectory === schemaDirectory ? "./_generated/functions.ts" : `../${schemaDirectory}/_generated/functions.ts`;
  await writeFile(resolve(root, functionsDirectory, "tasks.ts"), `import { query } from ${JSON.stringify(functionsImport)}\nexport const greeting = query(() => 'hello')\n`);
  return { root, config, output: resolve(root, schemaDirectory, "_generated") };
}

test("project generation bootstraps typed declarations, lazy server access and browser-safe discovered calls", async () => {
  const { root, config, output } = await project();
  try {
    await generateProject(config);
    const fetcher = spyOn(globalThis, "fetch").mockImplementation((async () => new Response(JSON.stringify({ code: "invalid_request" }), { status: 400, headers: { "content-type": "application/json" } })) as unknown as typeof fetch);
    try {
      const browser = await import(pathToFileURL(resolve(output, "client/runtime.ts")).href);
      const calls = await import(pathToFileURL(resolve(output, "client/tasks.ts")).href);
      expect(fetcher).not.toHaveBeenCalled();
      browser.setHeaders({ authorization: "Bearer app-token" });
      browser.setAuth({ key: "alice" });
      expect(fetcher).not.toHaveBeenCalled();
      await expect(Promise.resolve(calls.greeting())).rejects.toMatchObject({ code: "invalid_request" });
      expect(fetcher.mock.calls[0]?.[0]).toBe("http://localhost:9000/functions/session");
      expect(fetcher.mock.calls[0]?.[1]?.credentials).toBe("include");
      expect(new Headers(fetcher.mock.calls[0]?.[1]?.headers).get("authorization")).toBe("Bearer app-token");
      await browser.dispose();
    } finally { fetcher.mockRestore(); }
    const server = await readFile(resolve(output, "server.ts"), "utf8");
    expect(server).not.toContain("private-password");
    const generated = await import(pathToFileURL(resolve(output, "server.ts")).href);
    expect(typeof generated.db.select).toBe("function");
    expect(typeof generated.db.transaction).toBe("function");
    const anonymousTypes = resolve(root, "anonymous-check.ts");
    await writeFile(anonymousTypes, `import { query } from './db/_generated/functions.ts'
query((ctx) => { const identity: null = ctx.identity; return identity })
`);
    await expectTypechecks(anonymousTypes);
    const types = await readFile(resolve(output, "types.ts"), "utf8");
    expect(types).toContain("export type ReadDatabase");
    const client = resolve(output, "client/tasks.ts");
    const build = await Bun.build({ conditions: runtimeExportConditions(), entrypoints: [client], target: "browser" });
    expect(build.success).toBe(true);
    const bundle = (await Promise.all(build.outputs.map(output => output.text()))).join("\n");
    expect(bundle).not.toContain("private-password");
    expect(bundle).not.toContain("hello");
    expect(bundle).not.toContain("createDatabase");
    const manifest = JSON.parse(await readFile(resolve(output, "client/functions.json"), "utf8"));
    expect(Object.keys(manifest.functions)).toEqual(["tasks.greeting"]);
    const worker = await readFile(resolve(output, "worker.ts"), "utf8");
    expect(worker).toContain('"tasks": module0');
    expect(worker).not.toContain("db.config");
    expect(worker).not.toContain("private-password");
    const executable = await readFile(resolve(output, "worker.js"), "utf8");
    expect(executable).toContain("__hullaExecute");
    expect(executable).not.toContain("private-password");
    await generateProject(config);
  } finally { await rm(root, { recursive: true, force: true }); }
}, 30000);

test("generated declarations infer nullable provider identity without bundling auth code into workers or clients", async () => {
  const { root, config, output } = await project();
  try {
    await writeFile(config, `import { defineConfig } from '@hulla/db/config'
export default defineConfig({
  database: { server: { url: ${JSON.stringify(url)} } },
  auth: async (headers) => headers.get('authorization') === 'PRIVATE_AUTH_MARKER'
    ? { key: 'alice', identity: { id: 'alice', role: 'member' as const } }
    : null,
})
`);
    const declarations = resolve(root, "db/tasks.ts");
    await writeFile(declarations, `import { query } from './_generated/functions.ts'
export const greeting = query((ctx) => {
  // @ts-expect-error identity is nullable until checked
  ctx.identity.id
  if (ctx.identity === null) return 'anonymous'
  const id: string = ctx.identity.id
  const role: 'member' = ctx.identity.role
  // @ts-expect-error provider identity has no email field
  ctx.identity.email
  // @ts-expect-error identity fields retain their concrete types
  ctx.identity.id.toFixed()
  return id + role
})
`);
    await generateProject(config);
    await expectTypechecks(declarations);
    for (const [entry, target] of [["worker.ts", "bun"], ["client/tasks.ts", "browser"]] as const) {
      const build = await Bun.build({ conditions: runtimeExportConditions(), entrypoints: [resolve(output, entry)], target });
      expect(build.success).toBe(true);
      const bundle = (await Promise.all(build.outputs.map(output => output.text()))).join("\n");
      expect(bundle).not.toContain("PRIVATE_AUTH_MARKER");
      expect(bundle).not.toContain("private-password");
    }
  } finally { await rm(root, { recursive: true, force: true }); }
}, 30000);

test("project paths resolve relative to config and generation preserves authored files and symlinks", async () => {
  const { root, config, output } = await project("data", "endpoints");
  try {
    await generateProject(config);
    expect(await readFile(resolve(output, "client/tasks.ts"), "utf8")).toContain("tasks.greeting");
    expect(await readFile(resolve(output, "worker.ts"), "utf8")).toContain('"../../endpoints/tasks.ts"');
    const bundle = await readFile(resolve(output, "worker.js"), "utf8");
    await writeFile(resolve(output, "worker.js"), "// authored bundle\n");
    await expectCompilerError(generateProject(config), "Refusing to overwrite");
    await writeFile(resolve(output, "worker.js"), bundle);
    const generatedWorker = await readFile(resolve(output, "worker.ts"), "utf8");
    await writeFile(resolve(output, "worker.ts"), "// authored worker\n");
    await expectCompilerError(generateProject(config), "Refusing to overwrite");
    expect(await readFile(resolve(output, "worker.ts"), "utf8")).toBe("// authored worker\n");
    await rm(resolve(output, "worker.ts"));
    await symlink(resolve(root, "endpoints/tasks.ts"), resolve(output, "worker.ts"));
    await expectCompilerError(generateProject(config), "Refusing to overwrite");
    await rm(resolve(output, "worker.ts"));
    await writeFile(resolve(output, "worker.ts"), generatedWorker);
    await writeFile(resolve(output, "server.ts"), "// authored\n");
    await expectCompilerError(generateProject(config), "Refusing to overwrite");
    expect(await readFile(resolve(output, "server.ts"), "utf8")).toBe("// authored\n");
    await rm(output, { recursive: true });
    const external = resolve(root, "external");
    await mkdir(external);
    await symlink(external, output);
    await expectCompilerError(generateProject(config), "Refusing to overwrite");
    await rm(output);
    await mkdir(output);
    await symlink(external, resolve(output, "client"));
    await expectCompilerError(generateProject(config), "Refusing to overwrite");
  } finally { await rm(root, { recursive: true, force: true }); }
}, 30000);

test("unsupported transitive imports fail generation before declaration module evaluation", async () => {
  const { root, config } = await project();
  const key = `restricted-generation-${root}`;
  try {
    await writeFile(resolve(root, "db/helper.ts"), `import { readFileSync } from 'node:fs'; export const helper = () => readFileSync('secret', 'utf8');`);
    await writeFile(resolve(root, "db/tasks.ts"), `import { query } from './_generated/functions.ts';
import { helper } from './helper.ts';
Reflect.set(globalThis, ${JSON.stringify(key)}, true);
export const forbidden = query(() => helper());`);
    await expectCompilerError(generateProject(config), "Unsupported declaration import");
    expect(Reflect.has(globalThis, key)).toBe(false);
  } finally { Reflect.deleteProperty(globalThis, key); await rm(root, { recursive: true, force: true }); }
}, 30000);
