import assert from "node:assert/strict";
import { cp, mkdtemp, mkdir, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { postgresArchiveName, postgresAssetOrigin, postgresManifestName } from "./local-postgres.ts";

const root = Bun.fileURLToPath(new URL("../../", import.meta.url));
const version = (await Bun.file(join(root, "packages/db/package.json")).json()).version;
const archives = join(root, "output/releases", version);
const consumer = await mkdtemp(join(tmpdir(), "hulladb-release-consumer-"));
function command(args: string[], cwd = consumer) {
  const p = Bun.spawnSync(args, { cwd, stdout: "pipe", stderr: "pipe" });
  assert.equal(p.exitCode, 0, `${args.join(" ")}\n${p.stdout}\n${p.stderr}`);
}

async function verifyExports(name: string, packageRoot: string, exports: unknown) {
  if (typeof exports === "string") {
    assert(await Bun.file(join(packageRoot, exports)).exists(), `${name}: ${exports}`);
    return;
  }
  assert(exports && typeof exports === "object", `${name}: invalid export target`);
  for (const [condition, target] of Object.entries(exports)) {
    assert(condition !== "hulladb-source", `${name}: workspace export condition in archive`);
    await verifyExports(name, packageRoot, target);
  }
}
try {
  const dependencies: Record<string, string> = {
    typescript: "7.0.2", "@types/react": "^19.0.0", "@types/node": "26.2.0",
  };
  for (const [directory, name] of [["react", "react"], ["vue", "vue"], ["svelte", "svelte"], ["solid", "solid-js"], ["better-auth", "better-auth"]]) {
    dependencies[name!] = (await Bun.file(join(root, "packages", directory!, "node_modules", name!, "package.json")).json()).version;
  }
  // Content-address local archives so caches cannot substitute an older build.
  const directories: string[] = (await Bun.file(join(root, '.changeset/release.config.json')).json()).packages;
  const nativePackages = new Map<string, { tarball: ReturnType<typeof Bun.file>; manifest: Record<string, unknown> }>();
  for (const directory of directories) {
    const { name, os } = await Bun.file(join(root, directory, "package.json")).json() as { name: string; os?: string[] };
    const file = Bun.file(join(archives, `${name.replace("@", "").replace("/", "-")}-${version}.tgz`));
    if (os) {
      const result = Bun.spawnSync(["tar", "-xOf", file.name!, "package/package.json"]);
      assert.equal(result.exitCode, 0, result.stderr.toString());
      const archiveManifest = JSON.parse(result.stdout.toString());
      const integrity = "sha512-" + new Bun.CryptoHasher("sha512").update(await file.arrayBuffer()).digest("base64");
      nativePackages.set(name, { tarball: file, manifest: { ...archiveManifest, dist: { integrity } } });
      continue;
    }
    const hash = new Bun.CryptoHasher("sha256").update(await file.arrayBuffer()).digest("hex");
    const filename = `${name.replace('@', '').replace('/', '-')}-${hash}.tgz`;
    await Bun.write(join(consumer, filename), file);
    dependencies[name] = `file:./${filename}`;
  }
  await Bun.write(join(consumer, "package.json"), JSON.stringify({ private: true, type: "module", dependencies }));
  // Expose the two unpublished platform archives as registry dependencies so
  // npm must select the target even though the app only installs the adapter.
  const registry = Bun.serve({
    hostname: "127.0.0.1", port: 0,
    fetch(request) {
      const url = new URL(request.url);
      const name = decodeURIComponent(url.pathname).slice(1);
      if (url.pathname.startsWith("/tarballs/")) {
        const asset = nativePackages.get(name.slice("tarballs/".length));
        return asset ? new Response(asset.tarball) : new Response(null, { status: 404 });
      }
      const asset = nativePackages.get(name);
      if (!asset) return new Response(null, { status: 404 });
      const dist = { ...asset.manifest.dist as object, tarball: new URL(`tarballs/${encodeURIComponent(name)}`, url.origin).href };
      return Response.json({ name, "dist-tags": { latest: version }, versions: { [version]: { ...asset.manifest, dist } } });
    },
  });
  try {
    await Bun.write(join(consumer, ".npmrc"), `@hulla:registry=${registry.url.href}\n`);
    const install = Bun.spawn(["npm", "install", "--ignore-scripts", "--no-audit", "--no-fund", "--cache", join(consumer, ".npm-cache")], {
      cwd: consumer, stdout: "pipe", stderr: "pipe",
    });
    const [code, output, errors] = await Promise.all([install.exited, new Response(install.stdout).text(), new Response(install.stderr).text()]);
    assert.equal(code, 0, `npm install\n${output}\n${errors}`);
  } finally { await registry.stop(true); }
  console.log("Installed the release archives and selected a native runtime through the adapter's optional dependencies.");
  for (const name of Object.keys(dependencies).filter(name => name.startsWith("@hulla/"))) {
    const packageRoot = join(consumer, "node_modules", name);
    const manifest = await Bun.file(join(packageRoot, "package.json")).json();
    assert(!manifest.private && !manifest.scripts && !manifest.devDependencies);
    assert.equal(manifest.license, "MIT");
    assert.equal(manifest.publishConfig.access, "public");
    assert(await Bun.file(join(packageRoot, "LICENSE")).exists());
    await verifyExports(name, packageRoot, manifest.exports);
  }
  const assets = await Bun.file(join(consumer, "node_modules/@hulla/db-cli/dist/local-postgres-assets.json")).json();
  assert.deepEqual(assets, await Bun.file(join(archives, postgresManifestName)).json());
  for (const platform of ["darwin-arm64", "linux-x64"]) {
    const archive = postgresArchiveName(platform);
    const bytes = await Bun.file(join(archives, archive)).arrayBuffer();
    assert.deepEqual(assets[platform], {
      url: new URL(archive, postgresAssetOrigin).href,
      size: bytes.byteLength,
      sha256: new Bun.CryptoHasher("sha256").update(bytes).digest("hex"),
    });
  }
  const adapter = join(consumer, "node_modules/@hulla/db-postgres");
  const platform = `${process.platform}-${process.arch}`;
  const nativeName = `@hulla/db-postgres-${platform}`;
  const postgres = join(consumer, "node_modules", nativeName);
  const adapterManifest = await Bun.file(join(adapter, "package.json")).json();
  assert.deepEqual(adapterManifest.optionalDependencies, Object.fromEntries([...nativePackages.keys()].map(name => [name, version])));
  for (const name of nativePackages.keys()) {
    const binary = Bun.file(join(consumer, "node_modules", name, "native/hulladb-bridge"));
    assert.equal(await binary.exists(), name === nativeName, name);
  }
  await verifyExports(nativeName, postgres, (await Bun.file(join(postgres, "package.json")).json()).exports);
  assert(!await Bun.file(join(adapter, "native", platform, "hulladb-bridge")).exists());
  const notices = await Bun.file(join(postgres, "THIRD_PARTY_NOTICES.md")).text();
  for (const match of notices.matchAll(/\]\((licenses\/[^)]+)\)/g)) assert(await Bun.file(join(postgres, match[1]!)).exists());
  assert(notices.includes("rquickjs-sys"), "QuickJS native notices missing");
  assert(notices.includes("ring"), "TLS native notices missing");
  await mkdir(join(consumer, "db"));
  await Bun.write(join(consumer, "db/schema.ts"), `import { defineSchema, defineTable, v } from '@hulla/db/schema';\nimport {betterAuthTables} from '@hulla/db-better-auth/schema';\nexport default defineSchema({ ...betterAuthTables(), notes: defineTable({ title: v.string(), data: v.object({ rank: v.number() }) }) });\n`);
  await Bun.write(join(consumer, "db.config.ts"), `import {defineConfig} from '@hulla/db/config';\nexport default defineConfig({database:{server:{url:'postgresql://unused@127.0.0.1:1/unused'},client:{url:'https://database.example.com'}}});\n`);
  await Bun.write(join(consumer, "db/notes.ts"), `import {query,mutation} from './_generated/functions';\nexport const list=query(ctx=>ctx.db.select('notes'));\nexport const create=mutation(ctx=>ctx.db.insert('notes',{title:'Packaged',data:{rank:1}}));\n`);
  command(["node", "node_modules/@hulla/db-cli/dist/cli.js", "generate"]);
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
  command([process.execPath, join(root, "packages/db/tests/package.integration.ts"),
    ...["db", "db-sqlite", "db-postgres", "db-cli", `db-postgres-${platform}`].map(name => join(archives, `hulla-${name}-${version}.tgz`))], root);
  console.log("Installed native process and missing-asset recovery checks passed.");
} finally { await rm(consumer, { recursive: true, force: true }); }
