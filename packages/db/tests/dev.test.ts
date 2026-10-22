import { expect, test } from "bun:test";
import { spawn } from "node:child_process";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

test("development restarts fresh workers for changed, added and deleted sources, and recovers after invalid edits", async () => {
  const root = await mkdtemp(new URL("./dev-project-", import.meta.url).pathname);
  const external = await mkdtemp(new URL("./dev-functions-", import.meta.url).pathname);
  const scratch = await mkdtemp(resolve(tmpdir(), "hulla-dev-test-"));
  const events = resolve(scratch, "events.jsonl"), executable = resolve(scratch, "native-service");
  const delayStart = resolve(scratch, "delay-start");
  let output = "";
  let child: ReturnType<typeof spawn> | undefined;
  let exited: Promise<number | null> | undefined;
  async function records(): Promise<{ event: string; pid: number; manifest?: string }[]> {
    return (await readFile(events, "utf8").catch(() => "")).trim().split("\n").filter(Boolean).map(line => JSON.parse(line));
  }
  async function until(predicate: () => Promise<boolean>) {
    const deadline = Date.now() + 15_000;
    while (!await predicate()) {
      if (Date.now() >= deadline) throw new Error("Development condition timed out: " + output);
      await Bun.sleep(20);
    }
  }
  const starts = async () => (await records()).filter(item => item.event === "start");
  try {
    await mkdir(resolve(root, "db"));
    const configPath = resolve(root, "db.config.ts");
    const configSource = `import { defineConfig } from '@hulla/db/config'; export default defineConfig({database:{ server: {url:'postgresql://app@localhost/app'} }});`;
    await writeFile(configPath, configSource);
    await writeFile(resolve(root, "db/schema.ts"), `import { defineSchema, defineTable, v } from '@hulla/db/schema'; export default defineSchema({tasks:defineTable({title:v.string()})});`);
    const source = resolve(root, "db/tasks.ts");
    await writeFile(source, `import { query } from './_generated/functions.ts'; export const list = query(() => 'first');`);
    await writeFile(executable, `#!${process.execPath}
import { appendFileSync, existsSync, unlinkSync } from 'node:fs';
import { createInterface } from 'node:readline';
const log = event => appendFileSync(${JSON.stringify(events)}, JSON.stringify({...event,pid:process.pid})+'\\n');
const lines = createInterface({input:process.stdin});
for await (const line of lines) {
 const frame=JSON.parse(line);
 if(frame.type==='init') {log({event:'start',manifest:frame.manifest}); if(existsSync(${JSON.stringify(delayStart)})) {unlinkSync(${JSON.stringify(delayStart)});await Bun.sleep(400);} console.log(JSON.stringify({type:'ready',url:'http://127.0.0.1:39999'}));}
 if(frame.type==='shutdown') {await Bun.sleep(60);log({event:'stop'});break;}
}
lines.close();process.stdin.pause();
`, { mode: 0o755 });
    child = spawn(process.execPath, ["--conditions=hulladb-source", fileURLToPath(new URL("../src/cli.ts", import.meta.url)), "dev", resolve(root, "db.config.ts")], {
      env: { ...process.env, HULLADB_SERVICE: executable }, stdio: ["ignore", "pipe", "pipe"],
    });
    exited = new Promise(resolve => child!.once("exit", resolve));
    child.stdout!.on("data", value => { output += String(value); });
    child.stderr!.on("data", value => { output += String(value); });
    await until(async () => (await starts()).length === 1);
    // Generated output never feeds the watch/rebuild loop.
    await writeFile(resolve(root, "db/_generated/ignored.tmp"), "changed");
    await mkdir(resolve(root, ".next"));
    await writeFile(resolve(root, ".next/server.js"), "framework output");
    await writeFile(resolve(root, "runtime.log"), "log output");
    await writeFile(resolve(root, "runtime.log"), "more logs");
    await rm(resolve(root, "runtime.log"));
    await Bun.sleep(400);
    expect((await starts()).length).toBe(1);

    await writeFile(source, `import { query } from './_generated/functions.ts'; export const renamed = query(() => 'second');`);
    await until(async () => (await starts()).length === 2);
    expect((await starts()).at(-1)!.manifest).toContain("tasks.renamed");
    expect((await starts()).at(-1)!.manifest).not.toContain("tasks.list");
    // The next edit arrives while native startup is still pending, exercising
    // cleanup before startService has returned its close handle to the worker.
    await writeFile(delayStart, "delay");
    await mkdir(resolve(root, "db/nested"));
    await writeFile(resolve(root, "db/nested/extra.ts"), `import { query } from '../_generated/functions.ts'; export const extra = query(() => true);`);
    await until(async () => (await starts()).at(-1)!.manifest!.includes("nested/extra.extra"));
    await rm(resolve(root, "db/nested/extra.ts"));
    await until(async () => !(await starts()).at(-1)!.manifest!.includes("nested/extra.extra"));

    await writeFile(configPath, "export default ;");
    await until(async () => output.includes("Waiting for source changes before restarting."));
    const prior = (await starts()).length;
    await writeFile(configPath, configSource);
    await writeFile(source, `import { query } from './_generated/functions.ts'; export const fixed = query(() => 'recovered');`);
    await until(async () => (await starts()).length > prior);
    expect((await starts()).at(-1)!.manifest).toContain("tasks.fixed");
    // This directory is outside the supervisor's initial root. Only the worker's
    // IPC message can register its watcher; a lost message leaves edits unnoticed.
    const externalSource = resolve(external, "outside.ts");
    const constructors = JSON.stringify(relative(external, resolve(root, "db/_generated/functions.ts")));
    await writeFile(externalSource, `import { query } from ${constructors}; export const first = query(() => 'outside');`);
    await writeFile(configPath, `import { defineConfig } from '@hulla/db/config'; export default defineConfig({database:{ server: {url:'postgresql://app@localhost/app'} },functions:${JSON.stringify(external)}});`);
    await until(async () => (await starts()).at(-1)!.manifest!.includes("outside.first"));
    await writeFile(externalSource, `import { query } from ${constructors}; export const changed = query(() => 'outside edit');`);
    await until(async () => (await starts()).at(-1)!.manifest!.includes("outside.changed"));
    // Stop while a fresh worker is still evaluating config/generating. It must
    // never launch another native service after shutdown has been requested.
    const marker = resolve(scratch, "generating");
    const startCount = (await starts()).length;
    await writeFile(configPath, `import { writeFileSync } from 'node:fs';
writeFileSync(${JSON.stringify(marker)}, 'started');
await new Promise(resolve => setTimeout(resolve, 400));
${configSource}`);
    await until(async () => await readFile(marker, "utf8").catch(() => "") === "started");
    child.kill("SIGTERM");
    expect(await exited).toBe(0);
    expect((await starts()).length).toBe(startCount);
    const lifecycle = await records();
    expect(lifecycle.map(item => item.event)).toEqual(Array.from({ length: lifecycle.length }, (_, index) => index % 2 === 0 ? "start" : "stop"));
    expect(new Set(lifecycle.filter(item => item.event === "start").map(item => item.pid)).size).toBe(lifecycle.length / 2);
  } finally {
    child?.kill("SIGTERM");
    await exited;
    await rm(root, { recursive: true, force: true });
    await rm(external, { recursive: true, force: true });
    await rm(scratch, { recursive: true, force: true });
  }
}, 60_000);
