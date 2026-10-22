import { chmod, mkdir } from "node:fs/promises";
import { resolve } from "node:path";
import { executables, nativePlatforms, verifyNative } from "./native-artifacts.ts";

// Build on each supported target host, or stage genuine cross-built binaries
// with --from <directory> --platform linux-x64. No installation-time downloads.
const args = process.argv.slice(2);
let platform = `${process.platform}-${process.arch}`, from: string | undefined;
while (args.length) {
  const flag = args.shift(), value = args.shift();
  if (!value || (flag !== "--from" && flag !== "--platform")) throw new Error("Usage: build:native [--from directory --platform os-arch]");
  if (flag === "--from") from = resolve(value); else platform = value;
}
if (!(nativePlatforms as readonly string[]).includes(platform)) throw new Error(`Unsupported native platform: ${platform}`);
if (!from && platform !== `${process.platform}-${process.arch}`) throw new Error("Cross-built assets require --from; this command only compiles for the host");
const workspace = Bun.fileURLToPath(new URL("../../../", import.meta.url));
if (!from) {
  const result = Bun.spawnSync(["cargo", "build", "--release", "--locked", "-p", "hulladb_server", "--features", "typescript", "--bin", "hulladb-bridge", "--bin", "hulladb-service", "-p", "hulladb_js", "--bin", "hulladb-js"], { cwd: workspace, stdout: "inherit", stderr: "inherit" });
  if (result.exitCode !== 0) process.exit(result.exitCode);
  from = resolve(workspace, "target/release");
}
for (const name of executables) await verifyNative(resolve(from, name), platform);
const destination = new URL(`../native/${platform}/`, import.meta.url);
await mkdir(destination, { recursive: true });
for (const name of executables) {
  await Bun.write(new URL(name, destination), Bun.file(resolve(from, name)));
  await chmod(new URL(name, destination), 0o755);
}
