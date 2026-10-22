import { cp, mkdir, mkdtemp, readdir, rm } from "node:fs/promises";
import { join, relative, resolve } from "node:path";
import { tmpdir } from "node:os";
import { executables, verifyNative } from "../../packages/db/scripts/native-artifacts.ts";

const root = Bun.fileURLToPath(new URL("../../", import.meta.url));
const directories = ["db", "react", "vue", "svelte", "solid", "better-auth"];
const platforms = ["darwin-arm64", "linux-x64"];
function command(args: string[], cwd = root): string {
  const result = Bun.spawnSync(args, { cwd, stdout: "pipe", stderr: "pipe" });
  if (result.exitCode !== 0) throw new Error(`${args.join(" ")} failed:\n${result.stderr.toString()}${result.stdout.toString()}`);
  return result.stdout.toString();
}
command([process.execPath, "run", "build"]);
const version = (await Bun.file(join(root, "packages/db/package.json")).json()).version;
const output = resolve(root, "output/releases", version);
await mkdir(output, { recursive: true });
const staging = await mkdtemp(join(tmpdir(), "hulladb-npm-release-"));
try {
  for (const directory of directories) {
    const source = join(root, "packages", directory), stage = join(staging, directory);
    await mkdir(stage);
    const manifest = await Bun.file(join(source, "package.json")).json();
    if (manifest.version !== version || manifest.license !== "MIT") throw new Error(`Inconsistent release metadata: ${directory}`);
    // Source conditions and workspace build tooling are local development only.
    delete manifest.private; delete manifest.scripts; delete manifest.devDependencies;
    for (const conditions of Object.values(manifest.exports) as Record<string, string>[]) delete conditions["hulladb-source"];
    await cp(join(source, "dist"), join(stage, "dist"), { recursive: true });
    const readme = (await Bun.file(join(source, "README.md")).text()).replace(/\]\((\.\.\/[^)#]+)(#[^)]*)?\)/g,
      (_, path, fragment = "") => `](https://github.com/hulladev/db/blob/main/${relative(root, resolve(source, path))}${fragment})`);
    await Bun.write(join(stage, "README.md"), readme);
    await Bun.write(join(stage, "LICENSE"), Bun.file(join(root, "LICENSE")));
    if (directory === "db") {
      for (const platform of platforms) {
        for (const executable of executables) await verifyNative(join(source, "native", platform, executable), platform);
        await cp(join(source, "native", platform), join(stage, "native", platform), { recursive: true });
      }
      await nativeNotices(stage);
      manifest.files.push("THIRD_PARTY_NOTICES.md", "licenses");
    }
    await Bun.write(join(stage, "package.json"), JSON.stringify(manifest, null, 2) + "\n");
    const archive = `${manifest.name.replace("@", "").replace("/", "-")}-${version}.tgz`;
    command([process.execPath, "pm", "pack", "--ignore-scripts", "--quiet", "--filename", join(output, archive)], stage);
    console.log(`${manifest.name}: ${join(output, archive)}`);
  }
} finally { await rm(staging, { recursive: true, force: true }); }

async function nativeNotices(stage: string) {
  const metadata = JSON.parse(command(["cargo", "metadata", "--locked", "--format-version", "1"]));
  const dependencies = new Set<string>();
  for (const target of ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"]) {
    const tree = command(["cargo", "tree", "--locked", "-p", "hulladb_server", "-p", "hulladb_js", "--features", "hulladb_server/typescript", "--target", target, "--edges", "normal,build", "--prefix", "none", "--format", "{p}"]);
    for (const line of tree.split("\n")) { const [name, version] = line.split(" "); if (version) dependencies.add(`${name}@${version.slice(1)}`); }
  }
  const notices = ["# Native third-party notices", "", "The packaged native executables use the following third-party crates. Build dependencies are included. Original license, copyright and notice files are preserved below; this package's MIT license does not replace them.", ""];
  for (const dependency of metadata.packages.sort((a: any, b: any) => a.name.localeCompare(b.name))) {
    if (!dependency.source || !dependencies.has(`${dependency.name}@${dependency.version}`)) continue;
    const directory = resolve(dependency.manifest_path, "..");
    const paths: string[] = [];
    async function find(path: string) {
      for (const entry of await readdir(path, { withFileTypes: true })) {
        if (["target", ".git"].includes(entry.name)) continue;
        const full = join(path, entry.name);
        if (entry.isDirectory()) await find(full);
        else if (entry.isFile() && (/^(LICENSE|LICENCE|COPYING|COPYRIGHT|NOTICE)(\b|[-_.])/i.test(entry.name) || /(^|[/\\])licenses?[/\\]/i.test(relative(directory, full)))) paths.push(full);
      }
    }
    await find(directory);
    if (!paths.length) {
      // Some monorepo crates omit their shared license. Pin the upstream copy
      // to the commit recorded by Cargo, never a moving branch.
      const vcs = await Bun.file(join(directory, ".cargo_vcs_info.json")).json();
      const repository = dependency.repository?.replace(/\.git$/, "");
      if (!repository?.startsWith("https://github.com/") || !/^[0-9a-f]{40}$/.test(vcs.git?.sha1))
        throw new Error(`No pinned license source for ${dependency.name}@${dependency.version}`);
      let recovered = false;
      for (const name of ["LICENSE", "LICENSE.md", "LICENSE-MIT", "LICENSE-APACHE", "COPYING"]) {
        const url = `${repository.replace("github.com", "raw.githubusercontent.com")}/${vcs.git.sha1}/${name}`;
        const response = await fetch(url, { signal: AbortSignal.timeout(15_000) });
        if (response.status === 404) continue;
        if (!response.ok) throw new Error(`Cannot read upstream license: ${url} (${response.status})`);
        const local = join("licenses", `${dependency.name}-${dependency.version}`, name);
        await mkdir(resolve(stage, local, ".."), { recursive: true });
        await Bun.write(join(stage, local), await response.text());
        notices.push(`## ${dependency.name} ${dependency.version}`, "", `License: ${dependency.license}`, "", `- [Original notice](${local}), recovered from [upstream](${url}).`, "");
        recovered = true;
      }
      if (!recovered) throw new Error(`No original license files for ${dependency.name}@${dependency.version}`);
      continue;
    }
    notices.push(`## ${dependency.name} ${dependency.version}`, "", `License: ${dependency.license ?? "see original files"}`, "");
    for (const path of paths.sort()) {
      const local = join("licenses", `${dependency.name}-${dependency.version}`, relative(directory, path));
      await mkdir(resolve(stage, local, ".."), { recursive: true });
      await Bun.write(join(stage, local), Bun.file(path));
      notices.push(`- [${relative(directory, path)}](${local})`);
    }
    notices.push("");
  }
  await Bun.write(join(stage, "THIRD_PARTY_NOTICES.md"), notices.join("\n"));
}
