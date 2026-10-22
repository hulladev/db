/** Run explicitly after packing: bun tests/package.integration.ts /tmp/hulla-db-0.1.0.tgz */
import assert from "node:assert/strict";
import { copyFile, mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";

const tarball = process.argv[2];
if (!tarball) throw new Error("Pass the path to an npm-packed @hulla/db tarball");
const directory = await mkdtemp(resolve(tmpdir(), "hulladb-package-"));
const install = resolve(directory, "node_modules/@hulla/db");
try {
  await mkdir(install, { recursive: true });
  const unpack = Bun.spawnSync(["tar", "-xzf", resolve(tarball), "--strip-components=1", "-C", install]);
  assert.equal(unpack.exitCode, 0, unpack.stderr.toString());
  await copyFile(new URL("./fixtures/package-runtime.mjs", import.meta.url), resolve(directory, "check.mjs"));
  const nodePath = Bun.spawnSync(["node", "-p", "process.execPath"]).stdout.toString().trim();
  assert.ok(nodePath.startsWith("/"));
  const node = Bun.spawnSync([nodePath, "check.mjs"], { cwd: directory, env: { PATH: "/nonexistent", HOME: directory }, stdout: "inherit", stderr: "inherit" });
  assert.equal(node.exitCode, 0);
  await mkdir(resolve(directory, "generated"));
  for (const name of ["notes", "runtime"]) {
    await copyFile(new URL(`./generated/${name}.ts`, import.meta.url), resolve(directory, `generated/${name}.ts`));
  }
  await writeFile(resolve(directory, "browser.ts"), "export { create, pending } from './generated/notes';\n");
  const browser = await Bun.build({ entrypoints: [resolve(directory, "browser.ts")], target: "browser" });
  assert.ok(browser.success, String(browser.logs));
  const contents = await browser.outputs[0]!.text();
  for (const secret of ["node:child_process", "HULLADB_ENGINE", "native/", "database.url", "hulladb-source"]) assert.ok(!contents.includes(secret), secret);
  console.log("Installed browser export bundles without native or configuration modules");
} finally { await rm(directory, { recursive: true, force: true }); }
