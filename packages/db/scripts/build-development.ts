import { cp, mkdir, rm } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { nativePlatforms } from "./native-artifacts.ts";
import { developmentBun, verifyDevelopment } from "./development-artifacts.ts";

const root = Bun.fileURLToPath(new URL("../", import.meta.url));
const args = process.argv.slice(2);
let platform = `${process.platform}-${process.arch}`, from: string | undefined;
while (args.length) {
  const flag = args.shift(), value = args.shift();
  if (!value || !["--from", "--platform"].includes(flag!)) throw new Error("Usage: build:development [--from native-platform-directory --platform os-arch]");
  if (flag === "--from") from = resolve(value); else platform = value;
}
if (!(nativePlatforms as readonly string[]).includes(platform)) throw new Error(`Unsupported platform: ${platform}`);
if (!from && platform !== `${process.platform}-${process.arch}`) throw new Error("Build development assets on their target host, then stage with --from and --platform");
const compiler = dirname(Bun.fileURLToPath(import.meta.resolve("typescript/package.json")));
const destination = resolve(root, "native", platform);
await mkdir(destination, { recursive: true });
const shared = resolve(root, "development/compiler");
await rm(shared, { recursive: true, force: true });
await cp(compiler, shared, { recursive: true, dereference: true });
if (from) {
  for (const name of ["compiler", "postgres"]) {
    await rm(resolve(destination, name), { recursive: true, force: true });
    await cp(resolve(from, name), resolve(destination, name), { recursive: true, dereference: true });
  }
  for (const name of ["hulladb-dev", "BUN-LICENSE.md", "development.json"]) {
    const file = Bun.file(resolve(from, name));
    await Bun.write(resolve(destination, name), file, { mode: (await file.stat()).mode & 0o777 });
  }
} else {
  if (Bun.version !== developmentBun) throw new Error(`Build the distributed development runtime with Bun ${developmentBun}; installed Bun is ${Bun.version}.`);
  const nativeCompiler = dirname(Bun.resolveSync(`@typescript/typescript-${platform}/package.json`, compiler));
  await rm(resolve(destination, "compiler"), { recursive: true, force: true });
  await cp(nativeCompiler, resolve(destination, "compiler"), { recursive: true, dereference: true });
  const build = await Bun.build({ entrypoints: [resolve(root, "scripts/development-entry.ts")], compile: { outfile: resolve(destination, "hulladb-dev") } });
  if (!build.success) throw new Error(`Cannot build development runtime: ${build.logs.join("\n")}`);
  const license = await fetch(`https://raw.githubusercontent.com/oven-sh/bun/bun-v${Bun.version}/LICENSE.md`, { signal: AbortSignal.timeout(15_000) });
  if (!license.ok) throw new Error("Cannot preserve the bundled Bun runtime's license notice");
  await Bun.write(resolve(destination, "BUN-LICENSE.md"), await license.text());
  await Bun.write(resolve(destination, "development.json"), JSON.stringify({ bun: Bun.version, typescript: (await Bun.file(resolve(compiler, "package.json")).json()).version }) + "\n");
}
await verifyDevelopment(root, platform);
console.log(`Staged development runtime, compiler and PostgreSQL for ${platform}`);
