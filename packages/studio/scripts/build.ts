import { rm } from "node:fs/promises";
import { relative } from "node:path";
const root = Bun.fileURLToPath(new URL("../", import.meta.url));

await rm(new URL("../dist", import.meta.url), { recursive: true, force: true });
const compiler = Bun.spawn([process.execPath, "x", "--no-install", "tsc", "-p", "tsconfig.build.json"], {
  cwd: Bun.fileURLToPath(new URL("../", import.meta.url)), stdout: "inherit", stderr: "inherit",
  timeout: 30_000, killSignal: "SIGKILL",
});
if (await compiler.exited !== 0) process.exit(1);
const browserDirectory = Bun.fileURLToPath(new URL("../dist/", import.meta.url));
const build = await Bun.build({
  entrypoints: [new URL("../src/app.ts", import.meta.url).pathname],
  outdir: browserDirectory, target: "browser", format: "esm", splitting: true, minify: true,
  naming: { entry: "[name].[ext]", chunk: "chunks/[name]-[hash].[ext]" },
});
if (!build.success) throw new Error("Studio browser build failed", { cause: build.logs });
const scripts = build.outputs.map(output => relative(browserDirectory, output.path).replaceAll("\\", "/"));
await Bun.write(new URL("../dist/browser-assets.json", import.meta.url), JSON.stringify(scripts));
await Bun.write(new URL("../dist/licenses/highlight.js-LICENSE.txt", import.meta.url), Bun.file(new URL("./LICENSE", Bun.pathToFileURL(Bun.resolveSync("highlight.js/package.json", root)))));
const theme = await Bun.build({ entrypoints: [new URL("../src/theme.ts", import.meta.url).pathname], target: "browser", format: "iife", minify: true });
if (!theme.success) throw new Error("Studio theme build failed", { cause: theme.logs });
await Bun.write(new URL("../dist/theme.js", import.meta.url), theme.outputs[0]!);
await Bun.write(new URL("../dist/index.html", import.meta.url), Bun.file(new URL("../src/index.html", import.meta.url)));
await Bun.write(new URL("../dist/favicon.svg", import.meta.url), Bun.file(new URL("../src/favicon.svg", import.meta.url)));
const css = await Bun.build({
  entrypoints: [Bun.fileURLToPath(new URL("../src/style.css", import.meta.url))],
  minify: true, external: ["/fonts/*"],
});
if (!css.success) throw new Error("Studio CSS build failed", { cause: css.logs });
await Bun.write(new URL("../dist/style.css", import.meta.url), css.outputs[0]!);
for (const [name, asset] of [
  ["schibsted-grotesk", "@fontsource-variable/schibsted-grotesk/files/schibsted-grotesk-latin-wght-normal.woff2"],
  ["ibm-plex-mono", "@fontsource/ibm-plex-mono/files/ibm-plex-mono-latin-400-normal.woff2"],
]) {
  await Bun.write(new URL(`../dist/fonts/${name}.woff2`, import.meta.url), Bun.file(Bun.resolveSync(asset!, root)));
}
for (const [name, packageName] of [["schibsted-grotesk", "@fontsource-variable/schibsted-grotesk"], ["ibm-plex-mono", "@fontsource/ibm-plex-mono"]]) {
  await Bun.write(new URL(`../dist/fonts/${name}-LICENSE.txt`, import.meta.url), Bun.file(new URL("./LICENSE", Bun.pathToFileURL(Bun.resolveSync(`${packageName}/package.json`, root)))));
}
