import { expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

test("the portable database and Node driver preserve codecs and exact int64 values across reopen", async () => {
  const directory = await mkdtemp(join(tmpdir(), "hulla-node-adapter-"));
  try {
    const build = await Bun.build({ entrypoints: [new URL("./fixtures/node.ts", import.meta.url).pathname], target: "node", conditions: ["hulladb-source"], splitting: true, outdir: directory, naming: { entry: 'node.mjs', chunk: 'chunk-[hash].mjs' }, metafile: true });
    expect(build.success).toBe(true);
    for (const output of build.outputs) await Bun.write(output.path, output);
    const remote = Object.entries(build.metafile!.outputs)
      .filter(([, output]) => Object.keys(output.inputs).some(path => path.endsWith('/http.ts')))
      .map(([path]) => Bun.pathToFileURL(join(directory, path)).href);
    const path = join(directory, 'run.mjs');
    await Bun.write(path, `import{registerHooks}from'node:module';const remote=new Set(${JSON.stringify(remote)});registerHooks({load(url,context,next){if(remote.has(url))throw new Error('Local storage loaded the remote runtime');return next(url,context)}});await import('./node.mjs');`);
    const child = Bun.spawn([process.env.NODE ?? "node", path], { stdout: "pipe", stderr: "pipe", timeout: 15_000 });
    const [exit, stderr] = await Promise.all([child.exited, new Response(child.stderr).text()]);
    expect({ exit, stderr: exit === 0 ? "" : stderr }).toEqual({ exit: 0, stderr: "" });
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test("browser and Expo entrypoints bundle without native Node or Bun drivers", async () => {
  for (const platform of ["browser", "expo"] as const) {
    const build = await Bun.build({ entrypoints: [new URL(`../src/${platform}.ts`, import.meta.url).pathname], target: "browser", conditions: ["hulladb-source"], metafile: true });
    expect(build.success).toBe(true);
    expect(Object.keys(build.metafile!.inputs).some(path => /(?:node|bun)-driver/.test(path))).toBe(false);
    const output = (await Promise.all(build.outputs.map(item => item.text()))).join("\n");
    expect(output).not.toContain("node:sqlite");
    expect(output).not.toContain("bun:sqlite");
  }
});
