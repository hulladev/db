import assert from "node:assert/strict";
import { join } from "node:path";
const root = Bun.fileURLToPath(new URL("../../", import.meta.url));
const version = (await Bun.file(join(root, "packages/db/package.json")).json()).version;
const directories: string[] = (await Bun.file(join(root, '.changeset/release.config.json')).json()).packages;
for (const directory of directories) {
  const name: string = (await Bun.file(join(root, directory, 'package.json')).json()).name;
  const file = join(root, 'output/releases', version, `${name.replace('@', '').replace('/', '-')}-${version}.tgz`);
  const p = Bun.spawnSync(["npm", "publish", "--dry-run", "--ignore-scripts", "--json", file], { cwd: root, stdout: "pipe", stderr: "pipe" });
  assert.equal(p.exitCode, 0, `${p.stderr}\n${p.stdout}`);
  console.log(`Publish dry run passed: ${name}`);
}
