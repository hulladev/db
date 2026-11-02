/** Explicit installed-package check: bun tests/development.integration.ts (after build:development and build). */
import assert from "node:assert/strict";
import { access, mkdir, mkdtemp, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";

const packageRoot = Bun.fileURLToPath(new URL("../", import.meta.url));
const scratch = await mkdtemp(resolve(tmpdir(), "hulla-installed-dev-"));
const project = resolve(scratch, "project"), installed = resolve(project, "node_modules/@hulla/db");
const node = Bun.spawnSync(["node", "-p", "process.execPath"]).stdout.toString().trim();
assert(node.startsWith("/"));
const env = { PATH: "/nonexistent", HOME: scratch, NODE_ENV: "development" };
const listener = Bun.listen({ hostname: "127.0.0.1", port: 0, socket: { data() {} } });
const servicePort = listener.port;
listener.stop(true);
const cli = resolve(installed, "dist/cli.js");
interface Session { child: Bun.Subprocess<"ignore", "pipe", "pipe">; exited: Promise<number>; output: string; expectedExit: number }
const sessions: Session[] = [];
function command(args: string[], cwd: string, environment?: Record<string, string>) {
  const result = Bun.spawnSync(args, { cwd, ...(environment ? { env: environment } : {}), stdout: "pipe", stderr: "pipe", timeout: 30_000 });
  assert.equal(result.exitCode, 0, `${args[0]} failed:\n${result.stdout}\n${result.stderr}`);
  return result.stdout.toString().trim();
}
function start(root = project, databaseURL?: string): Session {
  const child = Bun.spawn([node, cli, "dev", resolve(root, "db.config.ts")], { cwd: scratch, env: { ...env, ...(databaseURL ? { DATABASE_URL: databaseURL } : {}) }, stdin: "ignore", stdout: "pipe", stderr: "pipe" });
  const session: Session = { child, exited: child.exited, output: "", expectedExit: 0 };
  async function capture(stream: ReadableStream<Uint8Array>) {
    const decoder = new TextDecoder();
    for await (const chunk of stream) session.output = (session.output + decoder.decode(chunk, { stream: true })).slice(-40_000);
    session.output = (session.output + decoder.decode()).slice(-40_000);
  }
  const drained = Promise.all([capture(child.stdout), capture(child.stderr)]);
  session.exited = child.exited.then(async code => { await drained; return code; });
  sessions.push(session);
  return session;
}
async function until(session: ReturnType<typeof start>, predicate: () => boolean | Promise<boolean>) {
  const deadline = Date.now() + 45_000;
  while (!await predicate()) {
    assert(session.child.exitCode === null && session.child.signalCode === null, session.output);
    assert(Date.now() < deadline, `Development condition timed out:\n${session.output}`);
    await Bun.sleep(30);
  }
}
async function stop(session: ReturnType<typeof start>) {
  if (session.child.exitCode === null && session.child.signalCode === null) session.child.kill("SIGTERM");
  assert.equal(await session.exited, session.expectedExit, session.output);
}
const imports = (root: string, path: string) => JSON.stringify(Bun.pathToFileURL(resolve(root, path)).href);
function app(script: string) { return command([node, "--input-type=module", "-e", script], scratch, env); }
const source = `import {query} from './_generated/functions';
export const list = query(ctx => ctx.db.select('notes'));
export const epoch = query(() => new Date(0));
export const greeting = query(() => 'first');\n`;
let schema = `import {defineSchema,defineTable,v} from '@hulla/db/schema';export default defineSchema({notes:defineTable({title:v.string()})});\n`;
const readyCount = (session: ReturnType<typeof start>) => session.output.split("Database service listening at ").length - 1;
function query(root = project) {
  return JSON.parse(app(`import {dispose} from ${imports(root, "db/_generated/client/runtime.ts")};
import {list,greeting,epoch} from ${imports(root, "db/_generated/client/notes.ts")};
try {console.log(JSON.stringify({rows:await list(),greeting:await greeting(),epoch:(await epoch()).toISOString()}));} finally {await dispose();}`));
}
try {
  await mkdir(installed, { recursive: true });
  const archive = resolve(scratch, "package.tgz");
  command([process.execPath, "pm", "pack", "--ignore-scripts", "--filename", archive], packageRoot);
  command(["tar", "-xzf", archive, "--strip-components=1", "-C", installed], scratch);
  await assert.rejects(access(resolve(project, "node_modules/typescript")));
  await mkdir(resolve(project, "db"));
  await Bun.write(resolve(project, "package.json"), JSON.stringify({ type: "module", dependencies: { "@hulla/db": "0.1.0" } }));
  await Bun.write(resolve(project, "db.config.ts"), `import {defineConfig} from '@hulla/db/config';export default defineConfig({database:{server:{}},service:{port:${servicePort}}});\n`);
  await Bun.write(resolve(project, "db/schema.ts"), schema);
  await Bun.write(resolve(project, "db/notes.ts"), source);
  command([resolve(installed, `native/${process.platform}-${process.arch}/hulladb-dev`), "generate", resolve(project, "db.config.ts")], scratch, env);
  const first = start();
  await until(first, () => readyCount(first) === 1);
  const state = resolve(project, ".hulla/local.json");
  const url = (await Bun.file(state).json()).url as string;
  assert.equal((await stat(state)).mode & 0o777, 0o600);
  assert.equal((await stat(resolve(project, ".hulla"))).mode & 0o777, 0o700);
  const pid = Number((await Bun.file(resolve(project, ".hulla/postgres/postmaster.pid")).text()).split("\n")[0]);
  // This application is a separate Node process, with neither a URL environment
  // variable nor the project's cwd. Generated imports must locate managed storage.
  app(`import {db} from ${imports(project, "db/_generated/server.ts")};try {await db.insert('notes',{title:'persisted'});} finally {await db.close();}`);
  assert.equal(query().rows[0].title, "persisted");
  assert.equal(query().epoch, "1970-01-01T00:00:00.000Z");
  await Bun.write(resolve(project, "db/notes.ts"), source.replace("'first'", "'reloaded'"));
  await until(first, () => readyCount(first) === 2);
  assert.equal(query().greeting, "reloaded");
  // A schema save must preserve populated storage and restart the service
  // without a second command or restarting the development supervisor.
  const beforeUpgrade = readyCount(first);
  schema = schema.replace("title:v.string()", "title:v.string(),done:v.boolean().default(false)");
  await Bun.write(resolve(project, "db/schema.ts"), schema);
  await until(first, () => readyCount(first) > beforeUpgrade);
  assert(first.output.includes("Applied development schema migration."));
  assert.equal(query().rows[0].title, "persisted");
  assert.equal(query().rows[0].done, false);
  assert.equal(Number((await Bun.file(resolve(project, ".hulla/postgres/postmaster.pid")).text()).split("\n")[0]), pid);
  // Invalid function code must fail generation before a pending schema upgrade
  // touches storage. A read-only plan proves the new field is still unapplied.
  const beforeInvalid = readyCount(first);
  const failures = first.output.split("Waiting for source changes").length;
  await Bun.write(resolve(project, "db/notes.ts"), "export const broken = ;");
  await Bun.write(resolve(project, "db/schema.ts"), schema.replace("title:v.string()", "title:v.string(),pending:v.boolean().default(true)"));
  await until(first, () => first.output.split("Waiting for source changes").length > failures);
  assert.equal(readyCount(first), beforeInvalid);
  const pending = command([node, cli, "migrate", resolve(project, "db.config.ts"), "--check"], scratch, env);
  assert(pending.includes('"pending"'));
  await Bun.write(resolve(project, "db/schema.ts"), schema);
  await Bun.write(resolve(project, "db/notes.ts"), source.replace("'first'", "'reloaded'"));
  await until(first, () => readyCount(first) > beforeInvalid);
  assert.equal(query().rows[0].done, false);
  const duplicate = start();
  await until(duplicate, () => duplicate.output.includes("Local database is locked"));
  await stop(duplicate);
  assert.equal(query().rows.length, 1);

  // An explicit URL must reuse PostgreSQL and leave provisioning to the caller.
  const external = resolve(scratch, "external");
  await mkdir(resolve(external, "db"), { recursive: true });
  await mkdir(resolve(external, "node_modules/@hulla"), { recursive: true });
  const { symlink } = await import("node:fs/promises");
  await symlink(installed, resolve(external, "node_modules/@hulla/db"));
  await Bun.write(resolve(external, "package.json"), '{"type":"module"}');
  const externalConfig = `import {defineConfig} from '@hulla/db/config';export default defineConfig({database:{server:{url:${JSON.stringify(url)},namespace:'external_dev'}},service:{port:0}});\n`;
  await Bun.write(resolve(external, "db.config.ts"), externalConfig);
  await Bun.write(resolve(external, "db/schema.ts"), schema);
  await Bun.write(resolve(external, "db/notes.ts"), source);
  const other = start(external);
  await until(other, () => other.output.includes("Waiting for source changes"));
  assert.equal(readyCount(other), 0, "external storage must not be provisioned automatically");
  command([node, cli, "init", resolve(external, "db.config.ts")], scratch, env);
  await Bun.write(resolve(external, "db.config.ts"), externalConfig + "// restart after explicit initialization\n");
  await until(other, () => readyCount(other) === 1);
  await assert.rejects(access(resolve(external, ".hulla")));
  // Even a supported change must not silently migrate an externally owned DB.
  const beforeExternalFailure = other.output.split("Waiting for source changes").length;
  const externalSchema = schema.replace("title:v.string()", "title:v.string(),externalExtra:v.boolean().default(true)");
  await Bun.write(resolve(external, "db/schema.ts"), externalSchema);
  await until(other, () => other.output.split("Waiting for source changes").length > beforeExternalFailure);
  assert.equal(readyCount(other), 1);
  const externalPlan = command([node, cli, "migrate", resolve(external, "db.config.ts"), "--check"], scratch, env);
  assert(externalPlan.includes("externalExtra"));
  assert(externalPlan.includes("No changes applied"));
  command([node, cli, "migrate", resolve(external, "db.config.ts")], scratch, env);
  await Bun.write(resolve(external, "db.config.ts"), externalConfig + "// restart after explicit migration\n");
  await until(other, () => readyCount(other) === 2);
  await stop(other);
  await Bun.write(resolve(external, "db.config.ts"), `import {defineConfig} from '@hulla/db/config';export default defineConfig({database:{server:{namespace:'external_dev'}},service:{port:0}});`);
  const fromEnvironment = start(external, url);
  await until(fromEnvironment, () => readyCount(fromEnvironment) === 1);
  await assert.rejects(access(resolve(external, ".hulla")));
  await stop(fromEnvironment);

  // A schema mismatch must stop the attempt without replacing local storage.
  const beforeMismatch = readyCount(first);
  const beforeMismatchFailures = first.output.split("Waiting for source changes").length;
  await Bun.write(resolve(project, "db/schema.ts"), schema.replace("title:v.string()", "title:v.string(),extra:v.boolean()"));
  await until(first, () => first.output.split("Waiting for source changes").length > beforeMismatchFailures);
  assert.equal(readyCount(first), beforeMismatch);
  await Bun.write(resolve(project, "db/schema.ts"), schema);
  await until(first, () => readyCount(first) > beforeMismatch);
  assert.equal(query().rows[0].title, "persisted");
  assert.equal(query().rows[0].done, false);
  await stop(first);
  assert.throws(() => process.kill(-pid, 0), { code: "ESRCH" });
  await assert.rejects(access(resolve(project, ".hulla/dev.lock")));
  const second = start();
  await until(second, () => readyCount(second) === 1);
  assert.equal((await Bun.file(state).json()).url, url);
  assert.equal(query().rows[0].title, "persisted");
  const secondPid = Number((await Bun.file(resolve(project, ".hulla/postgres/postmaster.pid")).text()).split("\n")[0]);
  second.expectedExit = 1;
  process.kill(secondPid, "SIGINT");
  assert.equal(await second.exited, 1, second.output);
  assert(second.output.includes("Local PostgreSQL stopped unexpectedly"));
  assert.throws(() => process.kill(-secondPid, 0), { code: "ESRCH" });
  const production = Bun.spawnSync([node, "--input-type=module", "-e", `await import(${imports(project, "db/_generated/server.ts")})`], {
    cwd: scratch, env: { ...env, NODE_ENV: "production" }, stdout: "pipe", stderr: "pipe",
  });
  assert.notEqual(production.exitCode, 0);
  assert(production.stderr.toString().includes("Production requires database.server.url or DATABASE_URL"));

  // Interrupt a fresh cluster while initdb/worker startup may still be in flight.
  // Capture descendants at the real process boundary, then prove none survives.
  const interrupted = resolve(scratch, "interrupted");
  await mkdir(resolve(interrupted, "db"), { recursive: true });
  await mkdir(resolve(interrupted, "node_modules/@hulla"), { recursive: true });
  await symlink(installed, resolve(interrupted, "node_modules/@hulla/db"));
  await Bun.write(resolve(interrupted, "package.json"), '{"type":"module"}');
  await Bun.write(resolve(interrupted, "db.config.ts"), `import {defineConfig} from '@hulla/db/config';export default defineConfig({database:{server:{}},service:{port:0}});`);
  await Bun.write(resolve(interrupted, "db/schema.ts"), schema);
  const early = start(interrupted);
  await until(early, async () => await access(resolve(interrupted, ".hulla/dev.lock/owner.json")).then(() => true, () => false));
  const processes = command(["ps", "-axo", "pid=,ppid="], scratch).split("\n").map(line => line.trim().split(/\s+/).map(Number));
  const owned = new Set([early.child.pid!]);
  let count = 0;
  while (count !== owned.size) {
    count = owned.size;
    for (const [pid, parent] of processes) if (owned.has(parent!)) owned.add(pid!);
  }
  await stop(early);
  for (const pid of owned) assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
  await assert.rejects(access(resolve(interrupted, ".hulla/dev.lock")));
  console.log("Installed Node CLI: generation, schema hot reload, preserved backfills, external migration checks, local persistence, schema mismatch and PostgreSQL cleanup passed without Bun/PostgreSQL on PATH.");
} finally {
  for (const session of sessions) await stop(session);
  await rm(scratch, { recursive: true, force: true });
}
