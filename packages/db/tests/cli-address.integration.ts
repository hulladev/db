/** Explicit native/PTY check after build:native, build:postgres and the Studio build:
 * bun --conditions=hulladb-source packages/db/tests/cli-address.integration.ts
 */
import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { resolve } from "node:path";

const root = await mkdtemp(new URL("./cli-address-project-", import.meta.url).pathname);
const cli = Bun.fileURLToPath(new URL("../src/cli.ts", import.meta.url));
const env: NodeJS.ProcessEnv = { ...process.env, NO_COLOR: "1", NODE_ENV: "development" };
// Exercise the same staged native assets as an ordinary checkout command.
for (const name of ["DATABASE_URL", "HULLADB_SERVICE", "HULLADB_ENGINE", "HULLADB_JS"]) delete env[name];
async function occupyPort() {
  for (let attempt = 0; attempt < 20; attempt++) {
    const server = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: () => new Response("occupied") });
    if (server.port! < 65534) return server;
    await server.stop(true);
  }
  throw new Error("Cannot reserve a test port with room for the next available address");
}
const occupiedService = await occupyPort();
const occupiedStudio = await occupyPort();
let output = "";
const decoder = new TextDecoder();
const terminal = new Bun.Terminal({ cols: 160, rows: 30, data(_terminal, data) { output = (output + decoder.decode(data, { stream: true })).slice(-40_000); } });
let child: Bun.Subprocess | undefined;
let postgresPID: number | undefined;
const serviceURLs = () => [...output.matchAll(/Database service listening at (http:\/\/127\.0\.0\.1:\d+)/g)].map(match => match[1]!);
const studioURLs = () => [...output.matchAll(/Hulla Studio listening at (http:\/\/127\.0\.0\.1:\d+\/#token=[\w-]+)/g)].map(match => match[1]!);
async function until(check: () => boolean) {
  const deadline = Date.now() + 30_000;
  while (!check()) {
    assert(child?.exitCode === null && child.signalCode === null, output);
    assert(!output.includes("Waiting for source changes before restarting."), output);
    assert(Date.now() < deadline, `Interactive startup timed out:\n${output}`);
    await Bun.sleep(20);
  }
}
function query() {
  const module = JSON.stringify(resolve(root, "db/_generated/client/notes.ts"));
  const runtime = JSON.stringify(resolve(root, "db/_generated/client/runtime.ts"));
  const result = Bun.spawnSync([process.execPath, "--conditions=hulladb-source", "-e", `
    import {list} from ${module}; import {dispose} from ${runtime};
    try {console.log(JSON.stringify(await list()));} finally {await dispose();}
  `], { env, stdout: "pipe", stderr: "pipe", timeout: 10_000, killSignal: "SIGKILL" });
  assert.equal(result.exitCode, 0, result.stderr.toString());
  assert.deepEqual(JSON.parse(result.stdout.toString()), []);
}
try {
  await mkdir(resolve(root, "db"));
  const config = resolve(root, "db.config.ts");
  await Bun.write(config, `export default {database:{server:{}},service:{port:${occupiedService.port}}};`);
  await Bun.write(resolve(root, "db/schema.ts"), `import {defineSchema,defineTable,v} from '@hulla/db/schema';export default defineSchema({notes:defineTable({title:v.string()})});`);
  const source = resolve(root, "db/notes.ts");
  const declaration = `import {query} from './_generated/functions';export const list=query(ctx=>ctx.db.select('notes'));\n`;
  await Bun.write(source, declaration);
  child = Bun.spawn([process.execPath, "--conditions=hulladb-source", cli, "dev", config, "--studio", "--studio-port", String(occupiedStudio.port)], { env, terminal });
  await until(() => output.split("Choose [1]: ").length === 2);
  assert(output.includes(`http://127.0.0.1:${occupiedService.port} is already in use`));
  terminal.write("\n");
  await until(() => output.split("Choose [1]: ").length === 3);
  assert(output.includes(`http://127.0.0.1:${occupiedStudio.port} is already in use`));
  terminal.write("\n");
  await until(() => studioURLs().length === 1);
  const service = serviceURLs()[0]!, studio = new URL(studioURLs()[0]!);
  assert(Number(new URL(service).port) > occupiedService.port!);
  assert(Number(studio.port) > occupiedStudio.port!);
  postgresPID = Number((await Bun.file(resolve(root, ".hulla/postgres/postmaster.pid")).text()).split("\n")[0]);
  query();
  const headers = { authorization: `Bearer ${new URLSearchParams(studio.hash.slice(1)).get("token")}` };
  assert.equal((await fetch(`${studio.origin}/api/schema`, { headers })).status, 200);
  await Bun.write(source, declaration + "// reload with the selected addresses\n");
  await until(() => studioURLs().length === 2);
  assert.deepEqual(serviceURLs(), [service, service]);
  assert.deepEqual(studioURLs(), [studio.href, studio.href]);
  assert.equal(output.split("Choose [1]: ").length, 3, "reload must not prompt again");
  query();
  console.log("Native CLI conflicts, default choices, generated client requests and reloads passed");
} finally {
  let exitCode: number | undefined;
  try {
    if (child) {
      if (child.exitCode === null && child.signalCode === null) child.kill("SIGTERM");
      const force = setTimeout(() => child!.kill("SIGKILL"), 20_000);
      try { exitCode = await child.exited; } finally { clearTimeout(force); }
    }
  } finally {
    terminal.close();
    await occupiedService.stop(true);
    await occupiedStudio.stop(true);
    // Check early startup failures too, before removing any disposable database data.
    const pidFile = Bun.file(resolve(root, ".hulla/postgres/postmaster.pid"));
    if (!postgresPID && await pidFile.exists()) postgresPID = Number((await pidFile.text()).split("\n")[0]);
    if (postgresPID) assert.throws(() => process.kill(postgresPID!, 0), { code: "ESRCH" });
    await rm(root, { recursive: true, force: true });
  }
  if (exitCode !== undefined) assert.equal(exitCode, 0, output);
}
