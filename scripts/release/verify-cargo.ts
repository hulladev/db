import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
const root = Bun.fileURLToPath(new URL("../../", import.meta.url));
const version = (await Bun.file(join(root, "packages/db/package.json")).json()).version;
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
  console.log("All nine extracted Cargo archives passed; no sibling source checkout was used.");
} finally { await rm(temporary, { recursive: true, force: true }); }
