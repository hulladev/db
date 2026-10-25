import { mkdir } from "node:fs/promises";
import { join } from "node:path";
const root = Bun.fileURLToPath(new URL("../../", import.meta.url));
const version = (Bun.TOML.parse(await Bun.file(join(root, "Cargo.toml")).text()) as { workspace: { package: { version: string } } }).workspace.package.version;
const npmVersion = (await Bun.file(join(root, "packages/db/package.json")).json()).version;
const cargoOnly = Bun.argv.includes("--cargo-only");
const directory = join(root, "output/releases", version);
await mkdir(directory, { recursive: true });
const cargoOrder = ["hulladb_schema", "hulladb_macros", "hulladb_js", "hulladb_postgres", "hulladb_server", "hulladb", "hulladb_protocol", "hulladb_wire", "hulladb_client"];
const npmOrder = cargoOnly ? [] : ["@hulla/db", "@hulla/db-react", "@hulla/db-vue", "@hulla/db-svelte", "@hulla/db-solid", "@hulla/db-better-auth"];
const artifacts: { name: string; file: string; bytes: number; sha256: string }[] = [];
for (const name of [...npmOrder, ...cargoOrder]) {
  const file = name.startsWith("@") ? `${name.replace("@", "").replace("/", "-")}-${npmVersion}.tgz` : `${name}-${version}.crate`;
  if (file.endsWith(".crate")) await Bun.write(join(directory, file), Bun.file(join(root, "target/package", file)));
  else if (npmVersion !== version) await Bun.write(join(directory, file), Bun.file(join(root, "output/releases", npmVersion, file)));
  const source = Bun.file(join(directory, file));
  artifacts.push({ name, file, bytes: source.size, sha256: new Bun.CryptoHasher("sha256").update(await source.arrayBuffer()).digest("hex") });
}
const head = Bun.spawnSync(["git", "rev-parse", "HEAD"], { cwd: root }).stdout.toString().trim();
const dirty = !!Bun.spawnSync(["git", "status", "--porcelain"], { cwd: root }).stdout.length;
await Bun.write(join(directory, "manifest.json"), JSON.stringify({ version, npmVersion, repository: "https://github.com/hulladev/db", license: "MIT", source: { head, dirty }, nativePlatforms: cargoOnly ? [] : ["darwin-arm64", "linux-x64"], publishOrder: { npm: npmOrder, cargo: cargoOrder }, artifacts }, null, 2) + "\n");
await Bun.write(join(directory, "SHA256SUMS"), artifacts.map(a => `${a.sha256}  ${a.file}`).join("\n") + "\n");
console.log(`Prepared ${artifacts.length} release archives in ${directory}`);
