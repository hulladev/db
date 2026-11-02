import { expect, test } from "bun:test";
import { cp, mkdtemp, mkdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

test("compiled standalone CLI resolves optional Studio from the application outside the checkout", async () => {
  const root = await mkdtemp(join(tmpdir(), "hulla-studio-consumer-"));
  const scope = join(root, "node_modules/@hulla");
  let child: Bun.Subprocess<"ignore", "pipe", "pipe"> | undefined;
  let drained: Promise<unknown> | undefined;
  let output = "";
  try {
    // Version-manager shims may fork the real runtime and exit first. Own the
    // actual Node process so lifecycle signals reach the listening server.
    const runtime = Bun.spawnSync([process.env.NODE ?? "node", "-p", "process.execPath"], { stdout: "pipe", stderr: "pipe" });
    expect(runtime.exitCode).toBe(0);
    const node = runtime.stdout.toString().trim();
    await mkdir(scope, { recursive: true });
    for (const name of ["db", "studio"]) {
      const source = fileURLToPath(new URL(`../../${name}/`, import.meta.url));
      const target = join(scope, name === "db" ? "db" : "db-studio");
      await cp(join(source, "dist"), join(target, "dist"), { recursive: true });
      const manifest = await Bun.file(join(source, "package.json")).json();
      delete manifest.devDependencies; delete manifest.scripts;
      for (const conditions of Object.values(manifest.exports) as Record<string, string>[]) delete conditions["hulladb-source"];
      await Bun.write(join(target, "package.json"), JSON.stringify(manifest));
    }
    await Bun.write(join(root, "package.json"), '{"type":"module","private":true}');
    await Bun.write(join(root, "db.config.js"), "export default {database:{server:{url:'postgresql://unused@127.0.0.1:1/unused'}},schema:'schema.js'};");
    await Bun.write(join(root, "schema.js"), "import {defineSchema,defineTable,v} from '@hulla/db/schema'; export default defineSchema({tasks:defineTable({title:v.string()})});");
    const cli = join(scope, "db/dist/cli.js");
    child = Bun.spawn([node, cli, "studio", "db.config.js", "--studio-port", "0"], {
      cwd: root, env: { ...process.env, NODE_ENV: "production" }, stdin: "ignore", stdout: "pipe", stderr: "pipe",
    });
    const collect = async (stream: ReadableStream<Uint8Array>) => {
      const decoder = new TextDecoder();
      for await (const chunk of stream) output += decoder.decode(chunk, { stream: true });
      output += decoder.decode();
    };
    drained = Promise.all([collect(child.stdout), collect(child.stderr)]);
    const deadline = Date.now() + 5000;
    while (!output.includes("Hulla Studio listening at")) {
      if (Date.now() > deadline || child.exitCode !== null) throw new Error(`Compiled Studio failed to start: ${output}`);
      await Bun.sleep(20);
    }
    const url = new URL(output.match(/http:\/\/127\.0\.0\.1:\d+\/#token=[\w-]+/)![0]);
    const headers = { Authorization: `Bearer ${new URLSearchParams(url.hash.slice(1)).get("token")}` };
    const metadata = await (await fetch(`${url.origin}/api/schema`, { headers })).json();
    expect(metadata.readOnly).toBe(true);
    expect(metadata.tables[0].name).toBe("tasks");
    const scripts = await Bun.file(join(scope, "db-studio/dist/browser-assets.json")).json() as string[];
    for (const file of scripts) {
      const response = await fetch(`${url.origin}/${file}`);
      expect(response.status).toBe(200);
      await response.arrayBuffer();
    }
    child.kill("SIGTERM"); expect(await child.exited).toBe(0); await drained;
    await expect(fetch(url.origin)).rejects.toThrow();
    await rm(join(scope, "db-studio"), { recursive: true });
    const missing = Bun.spawn([node, cli, "studio"], { cwd: root, stdout: "pipe", stderr: "pipe" });
    const error = await new Response(missing.stderr).text();
    expect(await missing.exited).toBe(1);
    expect(error).toContain("Studio requires @hulla/db-studio");
    const inert = Bun.spawn([node, "--input-type=module", "-e", "await import('@hulla/db/server')"], { cwd: root, stdout: "pipe", stderr: "pipe" });
    expect(await inert.exited).toBe(0);
  } finally {
    if (child && child.exitCode === null && child.signalCode === null) {
      child.kill("SIGTERM");
      const force = setTimeout(() => child!.kill("SIGKILL"), 3000);
      try { await child.exited; await drained; } finally { clearTimeout(force); }
    }
    await rm(root, { recursive: true, force: true });
  }
}, 12_000);
