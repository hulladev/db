import assert from "node:assert/strict";
import { cp, mkdtemp, mkdir, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";

const root = Bun.fileURLToPath(new URL("../../", import.meta.url));
const version = (await Bun.file(join(root, "packages/db/package.json")).json()).version;
const archives = join(root, "output/releases", version);
const consumer = await mkdtemp(join(tmpdir(), "hulladb-release-consumer-"));
function command(args: string[], cwd = consumer) {
  const p = Bun.spawnSync(args, { cwd, stdout: "pipe", stderr: "pipe" });
  assert.equal(p.exitCode, 0, `${args.join(" ")}\n${p.stdout}\n${p.stderr}`);
}
try {
  const dependencies: Record<string, string> = {
    typescript: "7.0.2", "@types/react": "^19.0.0", "@types/node": "26.2.0",
  };
  for (const [directory, name] of [["react", "react"], ["vue", "vue"], ["svelte", "svelte"], ["solid", "solid-js"], ["better-auth", "better-auth"]]) {
    dependencies[name!] = (await Bun.file(join(root, "packages", directory!, "node_modules", name!, "package.json")).json()).version;
  }
  // Content-address local archives so caches cannot substitute an older build.
  for (const directory of ["db", "react", "vue", "svelte", "solid", "better-auth"]) {
    const name = directory === "db" ? "@hulla/db" : `@hulla/db-${directory}`;
    const file = Bun.file(join(archives, `${name.replace("@", "").replace("/", "-")}-${version}.tgz`));
    const hash = new Bun.CryptoHasher("sha256").update(await file.arrayBuffer()).digest("hex");
    const filename = `${directory}-${hash}.tgz`;
    await Bun.write(join(consumer, filename), file);
    dependencies[name] = `file:./${filename}`;
  }
  await Bun.write(join(consumer, "package.json"), JSON.stringify({ private: true, type: "module", dependencies }));
  command(["npm", "install", "--ignore-scripts", "--no-audit", "--no-fund"]);
  console.log("Installed all six release archives outside the checkout.");
  for (const name of Object.keys(dependencies).filter(name => name.startsWith("@hulla/"))) {
    const packageRoot = join(consumer, "node_modules", name);
    const manifest = await Bun.file(join(packageRoot, "package.json")).json();
    assert(!manifest.private && !manifest.scripts && !manifest.devDependencies);
    assert.equal(manifest.license, "MIT");
    assert.equal(manifest.publishConfig.access, "public");
    assert(await Bun.file(join(packageRoot, "LICENSE")).exists());
    for (const conditions of Object.values(manifest.exports) as Record<string, string>[]) {
      assert(!("hulladb-source" in conditions));
      for (const path of Object.values(conditions)) assert(await Bun.file(join(packageRoot, path)).exists(), `${name}: ${path}`);
    }
  }
  const core = join(consumer, "node_modules/@hulla/db");
  const notices = await Bun.file(join(core, "THIRD_PARTY_NOTICES.md")).text();
  for (const match of notices.matchAll(/\]\((licenses\/[^)]+)\)/g)) assert(await Bun.file(join(core, match[1]!)).exists());
  assert(notices.includes("rquickjs-sys"), "QuickJS native notices missing");
  assert(notices.includes("ring"), "TLS native notices missing");
  await mkdir(join(consumer, "db"));
  await Bun.write(join(consumer, "db/schema.ts"), `import { defineSchema, defineTable, v } from '@hulla/db/schema';\nimport {betterAuthTables} from '@hulla/db-better-auth/schema';\nexport default defineSchema({ ...betterAuthTables(), notes: defineTable({ title: v.string(), data: v.object({ rank: v.number() }) }) });\n`);
  await Bun.write(join(consumer, "db.config.ts"), `import {defineConfig} from '@hulla/db/config';\nexport default defineConfig({database:{server:{url:'postgresql://unused@127.0.0.1:1/unused'},client:{url:'https://database.example.com'}}});\n`);
  await Bun.write(join(consumer, "db/notes.ts"), `import {query,mutation} from './_generated/functions';\nexport const list=query(ctx=>ctx.db.select('notes'));\nexport const create=mutation(ctx=>ctx.db.insert('notes',{title:'Packaged',data:{rank:1}}));\n`);
  command([process.execPath, "--bun", "node_modules/@hulla/db/dist/cli.js", "generate"]);
  await cp(join(root, "scripts/release/consumer.ts.txt"), join(consumer, "consumer.ts"));
  await Bun.write(join(consumer, "tsconfig.json"), JSON.stringify({ compilerOptions: { target: "ES2023", module: "ESNext", moduleResolution: "Bundler", strict: true, skipLibCheck: true, noEmit: true, allowImportingTsExtensions: true }, include: ["consumer.ts", "db"] }));
  await Bun.write(join(consumer, "server-consumer.ts"), `import {hullaAdapter} from '@hulla/db-better-auth';\nimport type {BetterAuthOptions} from 'better-auth';\nimport {db} from './db/_generated/server';\nexport const options: BetterAuthOptions = { database: hullaAdapter(db) };\n`);
  const tsconfig = await Bun.file(join(consumer, "tsconfig.json")).json();
  tsconfig.include.push("server-consumer.ts");
  await Bun.write(join(consumer, "tsconfig.json"), JSON.stringify(tsconfig));
  command([process.execPath, "x", "--no-install", "tsc", "--noEmit"]);
  const bundle = await Bun.build({ entrypoints: [join(consumer, "consumer.ts")], target: "browser", conditions: ["hulladb-source"] });
  assert(bundle.success, String(bundle.logs));
  for (const output of bundle.outputs) {
    const contents = await output.text();
    for (const privateImport of ["node:child_process", "HULLADB_ENGINE", "postgresql://unused"]) assert(!contents.includes(privateImport), privateImport);
  }
  console.log("Installed generation, all UI declaration consumers, browser boundaries and native license files passed.");
  // Existing behavioral smoke checks launch the packaged native processes under
  // Node with Cargo, checkout executables and PATH fallbacks unavailable.
  command([process.execPath, join(root, "packages/db/tests/package.integration.ts"), join(archives, `hulla-db-${version}.tgz`)], root);
  console.log("Installed native process and missing-asset recovery checks passed.");
} finally { await rm(consumer, { recursive: true, force: true }); }
