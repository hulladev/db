/** Explicit installed-package check after building the runtime and local PostgreSQL assets. */
import assert from "node:assert/strict";
import { access, cp, mkdir, mkdtemp, rm, stat, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { postgresArchiveName, postgresManifestName, preparePostgresArchives, reusePostgresArchives } from "../../../scripts/release/local-postgres.ts";
import { stagePostgresRuntime } from "../../../scripts/release/postgres-runtime.ts";

const packageRoot = Bun.fileURLToPath(new URL("../", import.meta.url));
const scratch = await mkdtemp(resolve(tmpdir(), "hulla-installed-dev-"));
const project = resolve(scratch, "project"), installed = resolve(project, "node_modules/@hulla/db");
const installedCLI = resolve(project, "node_modules/@hulla/db-cli");
const adapter = resolve(project, "node_modules/@hulla/db-postgres");
const node = Bun.spawnSync(["node", "-p", "process.execPath"]).stdout.toString().trim();
assert(node.startsWith("/"));
const tools = resolve(scratch, "bin");
const owner = process.env.HULLA_CODEX_PROCESS_OWNER;
const cache = resolve(scratch, "cache");
const env = { PATH: tools, HOME: scratch, XDG_CACHE_HOME: cache, NODE_ENV: "development", ...(owner ? { HULLA_CODEX_PROCESS_OWNER: owner } : {}) };
const listener = Bun.listen({ hostname: "127.0.0.1", port: 0, socket: { data() {} } });
const servicePort = listener.port;
listener.stop(true);
const cli = resolve(installedCLI, "dist/cli.js");
const platform = `${process.platform}-${process.arch}`;
const archiveName = postgresArchiveName(platform);
const nativePackage = `db-postgres-${platform}`;
let downloads = 0;
let otherDownloads = 0;
let downloadServer: Bun.Server<undefined> | undefined;
interface Session { child: Bun.Subprocess<"ignore", "pipe", "pipe">; exited: Promise<number>; output: string; expectedExit: number }
const sessions: Session[] = [];
function command(args: string[], cwd: string, environment?: Record<string, string>, timeout = 30_000) {
  const result = Bun.spawnSync(args, { cwd, ...(environment ? { env: environment } : {}), stdout: "pipe", stderr: "pipe", timeout });
  assert.equal(result.exitCode, 0, `${args[0]} failed:\n${result.stdout}\n${result.stderr}`);
  return result.stdout.toString().trim();
}
function start(root = project, databaseURL?: string, entry = 'db.config.ts', production = false): Session {
  const args = production
    ? [resolve(root, "node_modules/@hulla/db-postgres/dist/serve.js"), resolve(root, entry)]
    : [cli, "dev", resolve(root, entry)];
  const child = Bun.spawn([node, ...args], { cwd: scratch, env: { ...env, ...(production ? { NODE_ENV: "production", PATH: scratch } : {}), ...(databaseURL === undefined ? {} : { DATABASE_URL: databaseURL }) }, stdin: "ignore", stdout: "pipe", stderr: "pipe" });
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
function app(script: string, databaseURL?: string) { return command([node, "--input-type=module", "-e", script], scratch, { ...env, ...(databaseURL === undefined ? {} : { DATABASE_URL: databaseURL }) }); }
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
  await mkdir(tools);
  await symlink(process.execPath, resolve(tools, "bun"));
  await mkdir(project);
  await Bun.write(resolve(project, "package.json"), '{"type":"module","private":true}');
  const archives = [];
  for (const name of ["db", "db-postgres", "db-cli"]) {
    const archive = resolve(scratch, `${name}.tgz`);
    command([process.execPath, "pm", "pack", "--ignore-scripts", "--filename", archive], resolve(packageRoot, "..", name));
    archives.push(archive);
  }
  const runtime = resolve(scratch, "runtime");
  await stagePostgresRuntime(resolve(packageRoot, "../db-postgres/native"), resolve(runtime, "native"), platform);
  await cp(resolve(packageRoot, "../", nativePackage, "package.json"), resolve(runtime, "package.json"));
  const runtimeArchive = resolve(scratch, `${nativePackage}.tgz`);
  command([process.execPath, "pm", "pack", "--ignore-scripts", "--filename", runtimeArchive], runtime);
  archives.push(runtimeArchive);
  command(["npm", "install", "--ignore-scripts", "--no-audit", "--no-fund", "--no-package-lock", "--prefer-offline", ...archives], project, undefined, 120_000);
  const archive = Bun.file(resolve(scratch, archiveName));
  let otherArchive: Uint8Array<ArrayBuffer> | undefined;
  downloadServer = Bun.serve({
    hostname: "127.0.0.1", port: 0,
    async fetch(request) {
      if (new URL(request.url).pathname === "/other.tar.gz" && otherArchive) {
        otherDownloads++;
        return new Response(otherArchive);
      }
      if (new URL(request.url).pathname !== `/${archiveName}`) return new Response(null, { status: 404 });
      if (++downloads === 1) {
        const bytes = new Uint8Array(await archive.arrayBuffer());
        bytes[0] = bytes[0]! ^ 1;
        return new Response(bytes);
      }
      return new Response(archive);
    },
  });
  const assets = process.env.HULLADB_REUSE_POSTGRES_ASSETS === "true"
    ? await reusePostgresArchives(scratch, [platform])
    : await preparePostgresArchives(resolve(packageRoot, "../db-cli/native"), scratch, [platform], downloadServer.url.href);
  const asset = { ...assets[platform]!, url: new URL(archiveName, downloadServer.url).href };
  const manifestPath = resolve(installedCLI, "dist", postgresManifestName);
  await Bun.write(manifestPath, JSON.stringify({ [platform]: asset }));
  await mkdir(resolve(project, "db"));
  await Bun.write(resolve(project, "db.config.ts"), `import {defineConfig} from '@hulla/db/config';export default defineConfig({database:{server:{}},service:{port:${servicePort}}});\n`);
  await Bun.write(resolve(project, "db/schema.ts"), schema);
  await Bun.write(resolve(project, "db/notes.ts"), source);
  command([node, cli, "generate", resolve(project, "db.config.ts")], scratch, env);
  assert.equal(downloads, 0, "generation must work without downloading PostgreSQL");
  const rejected = start();
  await until(rejected, () => rejected.output.includes("PostgreSQL archive checksum mismatch"));
  await stop(rejected);
  await assert.rejects(access(resolve(cache, "hulla/db/postgres", `${platform}-${asset.sha256}.tar.gz`)), { code: "ENOENT" });
  await assert.rejects(access(resolve(project, ".hulla/postgres")), { code: "ENOENT" });
  await assert.rejects(access(resolve(project, ".hulla/dev.lock")), { code: "ENOENT" });
  const first = start();
  await until(first, () => readyCount(first) === 1);
  assert.equal(downloads, 2, "managed startup downloads a fresh archive after a failed verification");
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
  assert(first.output.includes("Applied development schema migration."), first.output);
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

  const productionRoot = resolve(scratch, "production");
  for (const [name, source] of [["db", installed], ["db-postgres", adapter],
    [nativePackage, resolve(project, "node_modules/@hulla", nativePackage)]] as const) {
    await cp(source, resolve(productionRoot, "node_modules/@hulla", name), { recursive: true });
  }
  await cp(resolve(project, "db"), resolve(productionRoot, "db"), { recursive: true });
  await Bun.write(resolve(productionRoot, "package.json"), '{"type":"module"}');
  await Bun.write(resolve(productionRoot, "db.config.ts"),
    `export default { database: { server: { url: ${JSON.stringify(url)} } }, service: { port: 0 } };`);
  const deployed = start(productionRoot, undefined, "db.config.ts", true);
  await until(deployed, () => readyCount(deployed) === 1);
  const deployedURL = deployed.output.match(/Database service listening at (http:\/\/127\.0\.0\.1:\d+)/)![1];
  assert.equal((await fetch(`${deployedURL}/functions/session`)).status, 200);
  await stop(deployed);

  // An explicit URL must reuse PostgreSQL and leave provisioning to the caller.
  const external = resolve(scratch, "external");
  await mkdir(resolve(external, "db"), { recursive: true });
  await mkdir(resolve(external, "node_modules/@hulla"), { recursive: true });
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

  // Explicit failures (including an empty environment value) must not select
  // local provisioning or reuse a different configured/environment connection.
  await Bun.write(resolve(external, 'db.config.ts'), `export default {database:{server:{url:'postgres://app:private-secret@127.0.0.1:1/app'}},service:{port:0}}`);
  const unreachable = start(external, url);
  await until(unreachable, () => unreachable.output.includes('Waiting for source changes'));
  assert.equal(readyCount(unreachable), 0);
  assert(!unreachable.output.includes('private-secret'));
  await assert.rejects(access(resolve(external, '.hulla')));
  await stop(unreachable);
  await Bun.write(resolve(external, 'db.config.ts'), `export default {database:{server:{}},service:{port:0}}`);
  const emptyConnection = start(external, '');
  await until(emptyConnection, () => emptyConnection.output.includes('Waiting for source changes'));
  await assert.rejects(access(resolve(external, '.hulla')));
  await stop(emptyConnection);

  // The adapter initializer is the dev entrypoint: no separate database config.
  const direct = resolve(scratch, 'direct');
  await mkdir(resolve(direct, 'db'), { recursive: true });
  await mkdir(resolve(direct, 'node_modules/@hulla'), { recursive: true });
  await symlink(installed, resolve(direct, 'node_modules/@hulla/db'));
  await symlink(adapter, resolve(direct, 'node_modules/@hulla/db-postgres'));
  await Bun.write(resolve(direct, 'package.json'), '{"type":"module"}');
  await Bun.write(resolve(direct, 'db/schema.ts'), `import {defineSchema,defineTable,v} from '@hulla/db';export const schema=defineSchema({notes:defineTable({title:v.string()})});`);
  await Bun.write(resolve(direct, 'db/functions.ts'), `import {schema} from './schema';export const list=schema.query(({db})=>db.select('notes'));export const create=schema.mutation(({db})=>db.insert('notes',{title:'adapter entrypoint'}));`);
  const initializer = (connection = '') => `import {postgres} from '@hulla/db-postgres';import {schema} from './schema';import * as functions from './functions';export const db=postgres({schema,functions,${connection}});`;
  const entry = resolve(direct, 'db/server.ts');
  await Bun.write(entry, initializer());
  const directClient = (session: Session, write: boolean) => {
    const baseUrl = [...session.output.matchAll(/Database service listening at (http:\/\/127\.0\.0\.1:\d+)/g)].at(-1)![1];
    return JSON.parse(app(`import {createClient} from ${imports(direct, 'db/_generated/server/_client.ts')};
const client=createClient({url:${JSON.stringify(baseUrl)}});
try{${write ? 'await client.functions.create();' : ''}console.log(JSON.stringify(await client.functions.list()));}finally{await client.dispose();}`));
  };
  // Change the gzip timestamp to simulate another valid build without changing its PostgreSQL data baseline.
  otherArchive = new Uint8Array(await archive.arrayBuffer());
  otherArchive[4] = otherArchive[4]! ^ 1;
  await Bun.write(manifestPath, JSON.stringify({ [platform]: {
    url: new URL("other.tar.gz", downloadServer.url).href,
    size: otherArchive.length,
    sha256: new Bun.CryptoHasher("sha256").update(otherArchive).digest("hex"),
  } }));
  const adapterManaged = start(direct, undefined, 'db/server.ts');
  await until(adapterManaged, () => readyCount(adapterManaged) === 1);
  assert.equal(otherDownloads, 1);
  await downloadServer.stop(true);
  downloadServer = undefined;
  const adapterState = resolve(direct, 'db/.hulla/local.json');
  assert.equal(typeof (await Bun.file(adapterState).json()).url, 'string');
  const adapterPID = Number((await Bun.file(resolve(direct, 'db/.hulla/postgres/postmaster.pid')).text()).split('\n')[0]);
  assert.equal(directClient(adapterManaged, true)[0].title, 'adapter entrypoint');
  await stop(adapterManaged);
  assert.throws(() => process.kill(adapterPID, 0), { code: 'ESRCH' });
  const adapterRestarted = start(direct, undefined, 'db/server.ts');
  await until(adapterRestarted, () => readyCount(adapterRestarted) === 1);
  assert.equal(directClient(adapterRestarted, false)[0].title, 'adapter entrypoint');
  await stop(adapterRestarted);
  await Bun.write(manifestPath, JSON.stringify({ [platform]: asset }));
  // Using an external endpoint still starts the service, but never initializes
  // that namespace until the explicit init command is run.
  await Bun.write(entry, initializer(`url:${JSON.stringify(url)},namespace:'adapter_external'`));
  const adapterExternal = start(direct, undefined, 'db/server.ts');
  await until(adapterExternal, () => adapterExternal.output.includes('Waiting for source changes'));
  assert.equal(readyCount(adapterExternal), 0);
  command([node, cli, 'init', entry], scratch, env);
  await Bun.write(entry, initializer(`url:${JSON.stringify(url)},namespace:'adapter_external'`) + '\n// initialized');
  await until(adapterExternal, () => readyCount(adapterExternal) === 1);
  assert.equal(directClient(adapterExternal, true)[0].title, 'adapter entrypoint');
  const adapterRows = JSON.parse(app(`import {postgres} from ${imports(project, 'node_modules/@hulla/db-postgres/dist/index.js')};
import {schema} from ${imports(direct, 'db/schema.ts')};
const db=postgres({schema,namespace:'adapter_external'});
try{await db.ready();console.log(JSON.stringify(await db.select('notes')));}finally{await db.close();}`, url));
  assert.equal(adapterRows[0].title, 'adapter entrypoint');
  const missingURL = app(`import {postgres} from ${imports(project, 'node_modules/@hulla/db-postgres/dist/index.js')};
import {schema} from ${imports(direct, 'db/schema.ts')};
const db=postgres({schema});try{await db.ready();throw new Error('Unexpected connection');}catch(error){if(!error.message.includes('No PostgreSQL connection'))throw error;console.log('missing URL rejected');}finally{await db.close();}`);
  assert.equal(missingURL, 'missing URL rejected');
  assert.throws(() => process.kill(adapterPID, 0), { code: 'ESRCH' });
  await stop(adapterExternal);

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
  console.log("Installed Node CLI: verified on-demand download, offline cache, managed and external PostgreSQL, schema migration, hot reload, persistence, and process cleanup passed. The production service ran without Bun or system PostgreSQL.");
} finally {
  const cleanup = await Promise.allSettled(sessions.map(stop));
  await downloadServer?.stop(true);
  await rm(scratch, { recursive: true, force: true });
  for (const result of cleanup) if (result.status === "rejected") throw result.reason;
}
