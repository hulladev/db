/** Run explicitly with the packed core, SQLite, PostgreSQL, and CLI archives. */
import assert from "node:assert/strict";
import { access, copyFile, mkdtemp, mkdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";

const [core, sqlite, postgres, cli] = process.argv.slice(2);
if (!core || !sqlite || !postgres || !cli) throw new Error("Pass the core, SQLite, PostgreSQL, and CLI npm archives");
const scratch = await mkdtemp(resolve(tmpdir(), "hulladb-package-"));
const directory = resolve(scratch, "runtime");
const install = resolve(directory, "node_modules/@hulla/db");
function unpack(name: string, destination: string, ...archives: string[]) {
  const result = Bun.spawnSync(["npm", "install", "--ignore-scripts", "--no-audit", "--no-fund", "--no-package-lock", "--prefer-offline", ...archives.map(archive => resolve(archive))],
    { cwd: destination, stdout: "pipe", stderr: "pipe" });
  assert.equal(result.exitCode, 0, `${name}: ${result.stderr}`);
}
try {
  await mkdir(directory);
  await Bun.write(resolve(directory, "package.json"), '{"type":"module","private":true}');
  unpack("core", directory, core);
  for (const entry of ["native", "development", "dist/service.js", "dist/cli.js"])
    await assert.rejects(access(resolve(install, entry)), { code: "ENOENT" });
  const manifest = await Bun.file(resolve(install, "package.json")).json();
  assert.deepEqual(manifest.dependencies ?? {}, {});
  const nodePath = Bun.spawnSync(["node", "-p", "process.execPath"]).stdout.toString().trim();
  assert.ok(nodePath.startsWith("/"));
  await mkdir(resolve(directory, "generated"));
  for (const name of ["notes", "runtime"]) {
    await copyFile(new URL(`./generated/${name}.ts`, import.meta.url), resolve(directory, `generated/${name}.ts`));
  }
  await Bun.write(resolve(directory, "browser.ts"), "export { create, pending } from './generated/notes';\n");
  const browser = await Bun.build({ entrypoints: [resolve(directory, "browser.ts")], target: "browser" });
  assert.ok(browser.success, String(browser.logs));
  const contents = await browser.outputs[0]!.text();
  for (const secret of ["node:child_process", "HULLADB_ENGINE", "native/", "database.url", "hulladb-source"]) assert.ok(!contents.includes(secret), secret);
  console.log("Installed browser export bundles without native or configuration modules");
  unpack("SQLite", directory, sqlite);
  for (const name of ["expo-crypto", "expo-sqlite", "@hulla/db-postgres", "@hulla/db-cli"])
    await assert.rejects(access(resolve(directory, "node_modules", name)), { code: "ENOENT" });
  const local = Bun.spawnSync([nodePath, "--input-type=module", "-e", `
    import assert from "node:assert/strict";
    import { defineSchema, defineTable, v } from "@hulla/db";
    import { sqlite } from "@hulla/db-sqlite";
    const schema = defineSchema({ notes: defineTable({ title: v.string() }) });
    const list = schema.query({ handler: ({ db }) => db.select("notes") });
    const db = sqlite.initialize({ schema, functions: { list }, filename: ":memory:" });
    try {
      const inserted = await db.insert("notes", { title: "Local" });
      assert.deepEqual(await db.functions.list(), [inserted]);
      assert.equal(inserted.title, "Local");
    }
    finally { await db.close(); }
  `], { cwd: directory, stdout: "inherit", stderr: "inherit" });
  assert.equal(local.exitCode, 0);
  console.log("Installed SQLite works without Expo, PostgreSQL, or development packages");
  unpack("PostgreSQL", directory, postgres);
  await assert.rejects(access(resolve(directory, "node_modules/@hulla/db-cli")), { code: "ENOENT" });
  await copyFile(new URL("./fixtures/package-runtime.mjs", import.meta.url), resolve(directory, "check.mjs"));
  const node = Bun.spawnSync([nodePath, "check.mjs"], { cwd: directory, env: { PATH: "/nonexistent", HOME: directory }, stdout: "inherit", stderr: "inherit" });
  assert.equal(node.exitCode, 0);

  const generator = resolve(scratch, "generation");
  await mkdir(generator);
  await Bun.write(resolve(generator, "package.json"), '{"type":"module","private":true}');
  unpack("CLI", generator, core, sqlite, cli);
  for (const entry of ["@hulla/db-local-postgres", "@hulla/db-postgres", "@hulla/db-cli/native"])
    await assert.rejects(access(resolve(generator, "node_modules", entry)), { code: "ENOENT" });
  await Bun.write(resolve(generator, "schema.ts"), `
    import { defineSchema, defineTable, v } from "@hulla/db";
    export const schema = defineSchema({ notes: defineTable({ title: v.string() }) });
  `);
  await Bun.write(resolve(generator, "db.ts"), `
    import { sqlite } from "@hulla/db-sqlite";
    import { schema } from "./schema.ts";
    export const db = sqlite({ schema, filename: "./unused.sqlite" });
  `);
  const generated = Bun.spawnSync([nodePath, "node_modules/@hulla/db-cli/dist/cli.js", "generate", "db.ts"],
    { cwd: generator, stdout: "inherit", stderr: "inherit" });
  assert.equal(generated.exitCode, 0);
  await access(resolve(generator, "_generated/db/source.ts"));
  console.log("Generation works with the installed compiler and Bun, without local PostgreSQL or packaged runtimes");
} finally { await rm(scratch, { recursive: true, force: true }); }
