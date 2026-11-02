import { cp, mkdir, mkdtemp, readdir, rm } from "node:fs/promises";
import { cpus, tmpdir } from "node:os";
import { join } from "node:path";

// Maintainer tooling only. Consumers receive these assets in the npm archive.
const version = "18.6";
const sha256 = "555610c24d53e4316da5b7d3fc25c279d96856d5e0e23ee308c328c5fa881d9f";
const platform = `${process.platform}-${process.arch}`;
const args = process.argv.slice(2);
if (args.length && (args.length !== 2 || args[0] !== "--source-archive")) throw new Error("Usage: build:postgres [--source-archive postgresql-18.6.tar.bz2]");
if (!["darwin-arm64", "darwin-x64", "linux-x64", "linux-arm64"].includes(platform)) throw new Error(`Unsupported PostgreSQL build host: ${platform}`);
const scratch = await mkdtemp(join(tmpdir(), "hulladb-postgres-build-"));
function command(args: string[], cwd: string) {
  const child = Bun.spawnSync(args, { cwd, stdout: "pipe", stderr: "pipe" });
  if (child.exitCode !== 0) throw new Error(`${args[0]} failed:\n${child.stderr.toString().slice(-12_000)}\n${child.stdout.toString().slice(-12_000)}`);
}
try {
  console.log(`Building local PostgreSQL ${version} for ${platform}`);
  const url = `https://ftp.postgresql.org/pub/source/v${version}/postgresql-${version}.tar.bz2`;
  const archive = join(scratch, "postgres.tar.bz2");
  if (args[1]) await Bun.write(archive, Bun.file(args[1]));
  else {
    const response = await fetch(url, { signal: AbortSignal.timeout(120_000) });
    if (!response.ok) throw new Error(`Cannot download PostgreSQL source (${response.status})`);
    await Bun.write(archive, response);
  }
  const file = Bun.file(archive);
  if (file.size > 40 * 1024 * 1024 || new Bun.CryptoHasher("sha256").update(await file.arrayBuffer()).digest("hex") !== sha256)
    throw new Error("PostgreSQL source checksum mismatch");
  command(["tar", "-xjf", archive, "-C", scratch], scratch);
  const source = join(scratch, `postgresql-${version}`), install = join(scratch, "install");
  // Loopback development needs no optional system libraries or TLS server.
  // PostgreSQL retains its own maintained SCRAM implementation.
  command(["./configure", `--prefix=${install}`, "--without-icu", "--without-readline", "--without-zlib", "--without-lz4", "--without-zstd", "--disable-nls"], source);
  command(["make", `-j${Math.min(cpus().length, 8)}`], source);
  command(["make", "install"], source);
  const destination = Bun.fileURLToPath(new URL(`../native/${platform}/postgres/`, import.meta.url));
  await mkdir(destination, { recursive: true });
  for (const name of ["bin", "lib", "share"]) await rm(join(destination, name), { recursive: true, force: true });
  await mkdir(join(destination, "bin"));
  for (const name of ["initdb", "postgres"]) await Bun.write(join(destination, "bin", name), Bun.file(join(install, "bin", name)), { mode: 0o755 });
  await mkdir(join(destination, "lib"));
  for (const name of await readdir(join(install, "lib"))) {
    if (!/\.(?:dylib|so(?:\.\d+)*)$/.test(name) || /^lib(?:ecpg|pgtypes)/.test(name)) continue;
    // npm archives omit symlinks. Preserve each loader name as a regular file.
    const file = Bun.file(join(install, "lib", name));
    await Bun.write(join(destination, "lib", name), file, { mode: (await file.stat()).mode & 0o777 });
  }
  if (process.platform === "darwin") {
    for (const [file, replacement] of [["bin/initdb", "@loader_path/../lib/libpq.5.dylib"], ["lib/libpqwalreceiver.dylib", "@loader_path/libpq.5.dylib"]]) {
      command(["install_name_tool", "-change", join(install, "lib/libpq.5.dylib"), replacement!, join(destination, file!)], source);
      command(["codesign", "--force", "--sign", "-", join(destination, file!)], source);
    }
    command(["install_name_tool", "-id", "@rpath/libpq.5.dylib", join(destination, "lib/libpq.5.dylib")], source);
    command(["codesign", "--force", "--sign", "-", join(destination, "lib/libpq.5.dylib")], source);
  }
  await cp(join(install, "share"), join(destination, "share"), { recursive: true, dereference: true });
  await Bun.write(join(destination, "COPYRIGHT"), Bun.file(join(source, "COPYRIGHT")));
  await Bun.write(join(destination, "build.json"), JSON.stringify({ version, source: url, sha256, optionalLibraries: false }, null, 2) + "\n");
  console.log(`Staged ${destination}`);
} finally { await rm(scratch, { recursive: true, force: true }); }
