import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
const root = Bun.fileURLToPath(new URL("../../", import.meta.url));
const version = (Bun.TOML.parse(await Bun.file(join(root, "Cargo.toml")).text()) as { workspace: { package: { version: string } } }).workspace.package.version;
const manifest = await Bun.file(join(root, "output/releases", version, "manifest.json")).json();
const crates: string[] = manifest.publishOrder.cargo;
const temporary = await mkdtemp(join(tmpdir(), "hulladb-release-crates-"));
try {
  for (const name of crates) {
    const file = join(root, "output/releases", version, `${name}-${version}.crate`);
    const unpack = Bun.spawnSync(["tar", "-xzf", file, "-C", temporary]);
    assert.equal(unpack.exitCode, 0, unpack.stderr.toString());
  }
  const members = crates.map(name => `${name}-${version}`);
  const patches = crates.map(name => `${name} = { path = ${JSON.stringify(`${name}-${version}`)} }`).join("\n");
  await Bun.write(join(temporary, "Cargo.toml"), `[workspace]\nresolver = "3"\nmembers = ${JSON.stringify(members)}\n\n[patch.crates-io]\n${patches}\n`);
  await Bun.write(join(temporary, "Cargo.lock"), Bun.file(join(root, "Cargo.lock")));
  console.log("Checking all production crate archive targets and features outside the checkout.");
  const check = Bun.spawn(["cargo", "check", "--workspace", "--all-features", "--all-targets", "--offline"], {
    cwd: temporary, env: { ...process.env, CARGO_TARGET_DIR: join(root, "target") }, stdout: "inherit", stderr: "inherit",
  });
  assert.equal(await check.exited, 0, "Extracted Cargo archive check failed");
  console.log(`All ${crates.length} extracted Cargo archives passed; no sibling source checkout was used.`);

  // Build normal consumers separately: workspace tests can unify optional features
  // and hide a missing production dependency or an accidentally enabled backend.
  const consumer = join(temporary, "consumer");
  await mkdir(join(consumer, "src"), { recursive: true });
  await Bun.write(join(consumer, "src/lib.rs"), Bun.file(join(root, "scripts/release/consumer.rs.txt")));
  const consumerPatches = crates.map(name => `${name} = { path = ${JSON.stringify(join(temporary, `${name}-${version}`))} }`).join("\n");
  for (const features of [[], ["http"], ["remote"], ["hosting"], ["http", "hosting", "legacy"]]) {
    const legacy = features.includes("legacy");
    const sdkFeatures = features.filter(feature => feature !== "legacy");
    const dependencies = [`hulladb = { version = "=${version}", default-features = false, features = ${JSON.stringify(sdkFeatures)} }`];
    if (legacy) for (const name of ["hulladb_schema", "hulladb_protocol", "hulladb_wire", "hulladb_postgres", "hulladb_server", "hulladb_client"]) {
      dependencies.push(`${name} = { version = "=${version}"${name === "hulladb_client" ? ', features = ["http"]' : ""} }`);
    }
    await Bun.write(join(consumer, "Cargo.toml"), `[package]\nname = "hulladb-release-consumer"\nversion = "0.0.0"\nedition = "2024"\n\n[workspace]\n\n[features]\nlegacy = []\n\n[dependencies]\n${dependencies.join("\n")}\n\n[patch.crates-io]\n${consumerPatches}\n`);
    const args = ["cargo", "check", "--offline", ...(legacy ? ["--features", "legacy"] : [])];
    const child = Bun.spawn(args, { cwd: consumer, env: { ...Bun.env, CARGO_TARGET_DIR: join(root, "target") }, stdout: "inherit", stderr: "inherit" });
    assert.equal(await child.exited, 0, `Archive consumer failed: ${features.join(",") || "no features"}`);
    const tree = Bun.spawnSync(["cargo", "tree", "--offline", "--edges", "normal", "--prefix", "none", "--format", "{p}"], { cwd: consumer });
    assert.equal(tree.exitCode, 0, tree.stderr.toString());
    const names = new Set(tree.stdout.toString().split("\n").map(line => line.split(" ")[0]));
    if (!legacy) for (const name of ["hulladb_client", "hulladb_server", "hulladb_protocol", "hulladb_wire"]) assert(!names.has(name), `SDK still depends on compatibility crate ${name}`);
    if (!sdkFeatures.includes("hosting")) for (const name of ["hulladb_postgres", "tokio-postgres", "axum"]) assert(!names.has(name), `Client-only build includes ${name}`);
    if (!sdkFeatures.includes("remote")) assert(!names.has("rusqlite"), "Transport-only build includes SQLite");
    console.log(`Archive consumer passed: ${features.join(",") || "no features"}`);
  }
} finally { await rm(temporary, { recursive: true, force: true }); }
