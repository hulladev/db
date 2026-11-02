import { expect, test } from "bun:test";
import { spawn } from "node:child_process";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

test("dev owns Studio, reloads schema and retains browser access across worker restarts", async () => {
  const root = await mkdtemp(fileURLToPath(new URL("./studio-dev-", import.meta.url)));
  let child: ReturnType<typeof spawn> | undefined, exited: Promise<number | null> | undefined;
  let output = "";
  const links = () => [...output.matchAll(/Hulla Studio listening at (http:\/\/127\.0\.0\.1:\d+\/#token=[\w-]+)/g)].map(match => new URL(match[1]!));
  async function until(check: () => boolean) {
    const deadline = Date.now() + 12_000;
    while (!check()) { if (Date.now() > deadline) throw new Error(`Development startup timed out: ${output}`); await Bun.sleep(20); }
  }
  try {
    await mkdir(`${root}/db`);
    await writeFile(`${root}/db.config.ts`, "export default {database:{server:{url:'postgresql://unused@127.0.0.1:1/unused'}}};");
    const schema = (extra = "") => `import {defineSchema,defineTable,v} from '@hulla/db/schema';export default defineSchema({tasks:defineTable({title:v.string()${extra}})});`;
    await writeFile(`${root}/db/schema.ts`, schema());
    // The native service boundary is controlled; Studio itself is a real HTTP
    // listener. Schema inspection must never try to open a database connection.
    await writeFile(`${root}/service`, `#!${process.execPath}
import {createInterface} from 'node:readline';
for await(const line of createInterface({input:process.stdin})) {const frame=JSON.parse(line);if(frame.type==='init') console.log(JSON.stringify({type:'ready',url:'http://127.0.0.1:39999'}));if(frame.type==='shutdown') process.exit(0);}
`, { mode: 0o700 });
    child = spawn(process.execPath, ["--conditions=hulladb-source", fileURLToPath(new URL("../../db/src/cli.ts", import.meta.url)), "dev", `${root}/db.config.ts`, "--studio", "--studio-port", "0"], {
      env: { ...process.env, HULLADB_SERVICE: `${root}/service` }, stdio: ["ignore", "pipe", "pipe"],
    });
    exited = new Promise((resolve, reject) => { child!.on("error", reject); child!.on("close", resolve); });
    child.stdout!.on("data", chunk => { output += chunk; }); child.stderr!.on("data", chunk => { output += chunk; });
    await until(() => links().length === 1);
    const first = links()[0]!, token = new URLSearchParams(first.hash.slice(1)).get("token");
    const headers = { Authorization: `Bearer ${token}` };
    expect((await fetch(`${first.origin}/api/schema`, { headers })).status).toBe(200);
    await writeFile(`${root}/db/schema.ts`, schema(",done:v.boolean().default(false)"));
    await until(() => links().length === 2);
    const second = links()[1]!;
    expect(second.hash).toBe(first.hash);
    expect(second.origin).toBe(first.origin);
    const metadata = await (await fetch(`${second.origin}/api/schema`, { headers })).json();
    expect(metadata.tables[0].fields.map((field: { name: string }) => field.name)).toContain("done");
    child.kill("SIGTERM");
    expect(await exited).toBe(0);
    await expect(fetch(second.origin)).rejects.toThrow();
  } finally {
    if (child && child.exitCode === null && child.signalCode === null) {
      child.kill("SIGTERM");
      const force = setTimeout(() => child!.kill("SIGKILL"), 8000);
      try { await exited; } finally { clearTimeout(force); }
    }
    await rm(root, { recursive: true, force: true });
  }
}, 30_000);
