import assert from "node:assert/strict";
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { chmod, mkdtemp, mkdir, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { createHash, randomUUID } from "node:crypto";
import { setTimeout as delay } from "node:timers/promises";
import { runCommand } from "./integration-runtime.ts";
import { resolve } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath, pathToFileURL } from "node:url";
import type { Database } from "../src/server.ts";
import type { SchemaDefinition } from "../src/schema.ts";

const url = process.env.HULLADB_TEST_URL!, namespace = process.env.HULLADB_TEST_NAMESPACE!;
if (!url || !namespace || !process.env.HULLADB_SERVICE) throw new Error("Run through the native service integration target");
const originalLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
const root = await mkdtemp(new URL("./service-project-", import.meta.url).pathname);
const cli = fileURLToPath(new URL("../src/cli.ts", import.meta.url));
const generated = resolve(root, "db/_generated");
const frozenDirectories: string[] = [];
const config = resolve(root, "db.config.ts"), credentials = resolve(root, "credentials.json");
let service: ChildProcessWithoutNullStreams | undefined, serviceExit: Promise<number | null> | undefined;
let db: Database<SchemaDefinition> | undefined, dispose: (() => Promise<void>) | undefined, stop: (() => void) | undefined;
let disposePages: (() => void) | undefined;
let stopSummary: (() => void) | undefined;
let output = "";
async function command(name: string, success = true) {
  const executable = name === "generate" ? process.env.BUN ?? "bun" : process.execPath;
  const { code, stdout, stderr } = await runCommand(executable, [cli, name, config]);
  assert.equal(code === 0, success, stderr);
  assert.equal((stdout + stderr).includes(url), false);
}
async function until(condition: () => boolean) {
  const deadline = Date.now() + 10_000;
  while (!condition()) { if (Date.now() > deadline) throw new Error("Service condition timed out"); await delay(10); }
}
async function digestGenerated(directory: string): Promise<Record<string, string>> {
  const result: Record<string, string> = {};
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) {
      for (const [name, digest] of Object.entries(await digestGenerated(path))) result[`${entry.name}/${name}`] = digest;
    } else result[entry.name] = createHash("sha256").update(await readFile(path)).digest("hex");
  }
  return result;
}
async function freezeGenerated(directory: string): Promise<Record<string, string>> {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) await freezeGenerated(path);
    else await chmod(path, 0o444);
  }
  await chmod(directory, 0o555);
  frozenDirectories.push(directory);
  return digestGenerated(directory);
}

try {
  await mkdir(resolve(root, "db"));
  await writeFile(credentials, JSON.stringify({ alice: true, bob: true }));
  await writeFile(config, `import { defineConfig } from '@hulla/db/config';
import { readFile } from 'node:fs/promises';
export default defineConfig({ database: ${JSON.stringify({ server: { url, namespace }, client: { url: "/" } })}, limits: { inputBytes: 2048, outputBytes: 4096 }, service: { port: 0, origins: ['https://app.example.test'] },
  auth: async headers => {
    const credential = headers.get('authorization');
    if (!credential) return null;
    if (credential === 'Bearer stalled') return await new Promise<never>(() => {});
    const user = credential === 'Bearer alice' ? 'alice' : credential === 'Bearer bob' ? 'bob' : null;
    const active = JSON.parse(await readFile(${JSON.stringify(credentials)}, 'utf8'));
    if (!user || !active[user]) throw new Error('Invalid credentials: provider details must remain private');
    return { key: user, identity: user };
  }
});`);
  await writeFile(resolve(root, "db/schema.ts"), `import { defineSchema, defineTable, sql, v } from '@hulla/db/schema';
class Amount { readonly cents: number; constructor(cents: number) { this.cents = cents; } }
const folders = defineTable({ slug: v.string() }, { primaryKey: 'slug' });
export const relations = defineTable({ folder: v.id(folders, { onDelete: 'cascade' }), owner: v.string(), title: v.string().min(2).max(40), low: v.int64().min(0), high: v.int64().min(0), state: v.string().oneOf(['draft', 'published']).default('draft') }, { checks: row => ({ valid_range: sql\`\${row.low} <= \${row.high}\` }) }).index(['owner', 'folder']);
const bookmarks = defineTable({ folder: v.id('Folders', { onDelete: 'setNull' }).nullable(), owner: v.string() });
const events = defineTable({ owner: v.string(), at: v.timestamp(), day: v.date(), precise: v.timestamp({ mode: 'string', precision: 6 }), safe: v.int64(), amount: v.number().codec({ encode: (value: Amount) => value.cents, decode: value => new Amount(value) }).default(new Amount(10)) });
export default defineSchema({ Folders: folders, relations, bookmarks, events, counters: defineTable({ owner: v.string(), key: v.string(), visits: v.int64({ mode: "bigint" }) }, { unique: { owner_key: { fields: ['owner', 'key'] } } }), metrics: defineTable({ owner: v.string(), amount: v.int64({ mode: "bigint" }) }), tasks: defineTable({ title: v.string(), owner: v.string() }), documents: defineTable({ owner: v.string(), payload: v.object({ tags: v.array(v.string()), nested: v.object({ score: v.number(), empty: v.string().nullable() }) }), score: v.number() }) });`);
  await writeFile(resolve(root, "db/tasks.ts"), `import { query, mutation } from './_generated/functions.ts';
import { tasks } from './_generated/tables.ts';
import type { Row } from './_generated/types.ts';
import { DatabaseError } from '@hulla/db';
import { createInsertSchema } from '@hulla/db/zod';
import { relations } from './schema.ts';
import { z } from 'zod';
import * as v from 'valibot';
export const intentionalFailure = mutation(async (ctx): Promise<string> => {
  await ctx.db.insert('tasks', { title: 'must roll back', owner: String(ctx.identity) });
  const failure = new DatabaseError({ code: 'unauthorized', message: 'This task is locked', details: { taskId: 'locked', reasons: [null, 'archived'] } });
  failure.stack = 'PRIVATE_STACK';
  Object.defineProperty(failure, 'cause', { value: new Error('PRIVATE_DRIVER') });
  throw failure;
});
export const failedQuery = query((): string => { throw new DatabaseError({ code: 'capacity', message: 'This query is disabled', details: null }); });
export const unexpected = query((): string => { throw new Error('PRIVATE_DRIVER'); });
export const oversizedError = query((): string => { throw new DatabaseError({ code: 'BIG', message: 'x'.repeat(16384) }); });
export const who = query(ctx => typeof ctx.identity === 'string' ? ctx.identity : null);
export const createRelation = mutation({
  input: createInsertSchema(relations).omit({ owner: true }),
  handler: (ctx, input) => {
    if (typeof ctx.identity !== 'string') throw new Error('Sign in required');
    return ctx.db.insert('relations', { ...input, owner: ctx.identity });
  },
});
export const related = query(async ctx => {
  if (typeof ctx.identity !== 'string') throw new Error('Sign in required');
  return { relations: await ctx.db.select('relations').where({ owner: ctx.identity }), bookmarks: await ctx.db.select('bookmarks').where({ owner: ctx.identity }) };
});
export const list = query(async (ctx): Promise<Row<'tasks'>[]> => {
  if (typeof ctx.identity !== 'string') throw new Error('Sign in required');
  return await ctx.db.select(tasks).where(({ and, eq, not }) => and(eq(tasks.owner, ctx.identity), not(eq(tasks.title, 'excluded')))).orderBy(tasks.title);
});
export const paged = query({
  input: z.object({ limit: z.number().int().min(1).max(1000), cursor: z.string().nullable() }),
  handler: (ctx, input) => {
    if (typeof ctx.identity !== 'string') throw new Error('Sign in required');
    return ctx.db.select(tasks, { id: true, title: true }).where('owner', '=', ctx.identity)
      .where('title', 'startsWith', 'page:').orderBy('title').paginate(input);
  },
});
export const create = mutation(async (ctx): Promise<Row<'tasks'>> => {
  if (typeof ctx.identity !== 'string') throw new Error('Sign in required');
  return await ctx.db.insert('tasks', { title: 'From browser', owner: ctx.identity });
});
export const validated = mutation({
  input: z.object({ title: z.string().transform(async title => title.trim()) }),
  output: z.object({ title: z.string().min(1) }).transform(async value => ({ label: value.title.toUpperCase() })),
  handler: async (ctx, input) => {
    if (!ctx.identity) throw new Error('Sign in required');
    const row = await ctx.db.insert('tasks', { title: input.title, owner: ctx.identity });
    return { title: row.title };
  },
});
export const valibot = query({
  input: v.pipeAsync(v.object({ title: v.string() }), v.transformAsync(async value => ({ title: value.title.trim() }))),
  handler: (_ctx, input) => ({ label: input.title.toLowerCase() }),
});
export const metricsSummary = query(ctx => {
  if (typeof ctx.identity !== 'string') throw new Error('Sign in required');
  return ctx.db.select('metrics').where('owner', '=', ctx.identity).aggregate(a => ({ count: a.count(), total: a.sum('amount') }));
});
export const metricGroups = query(ctx => {
  if (typeof ctx.identity !== 'string') throw new Error('Sign in required');
  return ctx.db.select('metrics').where('owner', '=', ctx.identity).groupBy('owner')
    .aggregate(a => ({ count: a.count(), total: a.sum('amount') }))
    .having(({ gt }) => gt('total', 9223372036854775807n)).orderBy('total', 'desc').limit(2);
});
export const visit = mutation({
  input: z.object({ key: z.string(), fail: z.boolean() }),
  handler: (ctx, input) => {
    if (typeof ctx.identity !== 'string') throw new Error('Sign in required');
    return ctx.db.insert('counters', { owner: ctx.identity, key: input.key, visits: 1n })
      .onConflict(({ on }) => on('owner', 'key').update(({ current, incoming }) => ({ visits: current.visits.add(incoming.visits) })))
      .returning(['key', 'visits']).expectRows(input.fail ? 2 : 1);
  },
});
export const caughtCount = mutation(async ctx => {
  if (typeof ctx.identity !== 'string') throw new Error('Sign in required');
  await ctx.db.insert('counters', { owner: ctx.identity, key: 'before failed count', visits: 1n });
  try { await ctx.db.delete('counters').where({ owner: ctx.identity, key: 'missing' }).expectRows(1); } catch {}
  return 'caught';
});
export const skipVisit = mutation(ctx => {
  if (typeof ctx.identity !== 'string') throw new Error('Sign in required');
  return ctx.db.insert('counters', { owner: ctx.identity, key: 'guide', visits: 100n })
    .onConflict(({ on }) => on('owner', 'key').skip()).returning(['key']).expectRows(0);
});
export const createEvent = mutation({
  input: z.object({ when: z.date() }),
  handler: async (ctx, input) => {
    if (typeof ctx.identity !== 'string') throw new Error('Sign in required');
    const event = await ctx.db.insert('events', { owner: ctx.identity, at: input.when, day: '2026-10-20', precise: '2026-10-20T12:34:56.123456Z', safe: 42 });
    return { id: event.id, when: event.at, day: event.day, precise: event.precise, safe: event.safe, amount: event.amount.cents };
  },
});
export const eventDates = query(ctx => {
  if (typeof ctx.identity !== 'string') throw new Error('Sign in required');
  return ctx.db.select('events', ['at']).where('owner', '=', ctx.identity);
});
export const limitInput = mutation({
  input: z.string().transform(() => { throw new Error('Input validation ran before the byte limit'); }),
  handler: ctx => ctx.db.insert('tasks', { title: 'limit: invalid input', owner: 'bob' }).returning(['id']),
});
export const limitOutput = mutation({
  limits: { outputBytes: 16 },
  handler: async ctx => {
    await ctx.db.insert('tasks', { title: 'limit: invalid output', owner: 'bob' });
    return 'x'.repeat(32);
  },
});
export const raisedLimits = mutation({
  input: z.string(), limits: { inputBytes: 4096, outputBytes: 8192 },
  handler: (_ctx, input) => input + input,
});
export const largePayload = query({
  input: z.string(),
  limits: { inputBytes: 20 * 1024 * 1024, outputBytes: 20 * 1024 * 1024, memoryBytes: 256 * 1024 * 1024 },
  handler: (_ctx, input) => input,
});
export const limitReads = mutation({
  limits: { readRows: 1 },
  handler: async ctx => {
    await ctx.db.insert('tasks', { title: 'limit: read rollback', owner: 'bob' });
    try { await ctx.db.select('tasks'); } catch {}
    return 'caught';
  },
});
export const limitOperations = mutation({
  limits: { operations: 1 },
  handler: async ctx => {
    await ctx.db.insert('tasks', { title: 'limit: operation rollback', owner: 'bob' });
    await ctx.db.select('tasks');
    return 'finished';
  },
});
export const limitExecution = mutation({
  limits: { executionMs: 200 },
  handler: async ctx => {
    await ctx.db.insert('tasks', { title: 'limit: execution rollback', owner: 'bob' });
    while (true) {}
    return 'unreachable';
  },
});
export const jsonWrite = mutation({
  input: z.object({ payload: z.object({ tags: z.array(z.string()), nested: z.object({ score: z.number(), empty: z.null() }) }), score: z.number() }),
  handler: (ctx, input) => {
    if (typeof ctx.identity !== 'string') throw new Error('Sign in required');
    return ctx.db.insert('documents', { ...input, owner: ctx.identity }).returning(['payload', 'score']);
  },
});
export const jsonList = query(ctx => {
  if (typeof ctx.identity !== 'string') throw new Error('Sign in required');
  return ctx.db.select('documents').where('owner', '=', ctx.identity);
});
export function helper() { return 'not an endpoint'; }`);
  await command("generate");
  // Compile the actual generated consumer: runtime-only checks would miss an
  // adapter input that cannot be passed to the schema-bound database types.
  await writeFile(resolve(root, 'tsconfig.json'), JSON.stringify({ extends: '../../tsconfig.json', include: ['db/**/*.ts'], exclude: [] }));
  const checked = await runCommand(process.execPath, [fileURLToPath(new URL('../node_modules/typescript/bin/tsc', import.meta.url)), '--noEmit', '--project', resolve(root, 'tsconfig.json')]);
  assert.equal(checked.code, 0, checked.stdout + checked.stderr);
  const generatedArtifacts = await freezeGenerated(generated);
  // Deployment has only compiled declarations: imports of the source would fail.
  const declarationSource = resolve(root, "db/tasks.ts");
  await writeFile(declarationSource, "throw new Error('Declaration source must not execute while serving');\n" + await readFile(declarationSource, "utf8"));
  await command("serve", false); // Serving verifies storage; it must never provision it.
  await command("init");
  service = spawn(process.execPath, ["--conditions=hulladb-source", cli, "serve", config], { stdio: ["pipe", "pipe", "pipe"] });
  serviceExit = new Promise(resolve => service!.once("close", resolve));
  service.stderr.on("data", chunk => { output += String(chunk); });
  const lines = createInterface({ input: service.stdout });
  const endpoint = await new Promise<string>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("CLI startup timed out: " + output)), 30_000);
    lines.on("line", line => {
      output += line + "\n";
      if (line.startsWith("Database service listening at ")) { clearTimeout(timer); resolve(line.slice("Database service listening at ".length)); }
    });
    service!.once("error", reject);
    service!.once("exit", () => { clearTimeout(timer); reject(new Error("CLI exited: " + output)); });
  });
  Object.defineProperty(globalThis, "location", { configurable: true, value: { origin: endpoint } });
  const browser = await import(pathToFileURL(resolve(generated, "client/runtime.ts")).href);
  const calls = await import(pathToFileURL(resolve(generated, "client/tasks.ts")).href);
  db = (await import(pathToFileURL(resolve(generated, "server.ts")).href)).db;
  let bearer: string | undefined;
  dispose = browser.dispose;
  browser.setHeaders(() => bearer ? { authorization: `Bearer ${bearer}` } : {});
  assert.equal(await calls.who(), null);
  await assert.rejects(Promise.resolve(calls.create()));
  bearer = "alice";
  // An app cache key cannot impersonate Bob: only the verified credential governs ctx.identity.
  browser.setAuth({ key: "bob-forged-client-metadata" });
  assert.equal(await calls.who(), "alice");
  const states: { status: string; data?: { title: string; owner: string }[] }[] = [];
  stop = calls.list.subscribe((state: typeof states[number]) => states.push(state));
  await until(() => states.at(-1)?.status === "success");
  const created = await calls.create();
  assert.equal(created.owner, "alice");
  await until(() => states.at(-1)?.data?.length === 1);
  await db!.insert("tasks", { title: "From server", owner: "alice" });
  await until(() => states.at(-1)?.data?.some(task => task.title === "From server") === true);

  const forbidden = await fetch(endpoint + "/functions/session", { headers: { authorization: "Bearer invalid" } });
  assert.equal(forbidden.status, 401);
  assert.equal((await forbidden.text()).includes("provider details"), false);
  const unavailable = await fetch(endpoint + "/functions/session", { headers: { authorization: "Bearer stalled" } });
  assert.equal(unavailable.status, 503, "a verifier outage must not be reported as invalid credentials");
  assert.equal(await calls.who(), "alice", "later valid verification must remain available");
  const cors = await fetch(endpoint + "/functions/query", { method: "OPTIONS", headers: { origin: "https://app.example.test", "access-control-request-method": "POST" } });
  assert.equal(cors.headers.get("access-control-allow-origin"), "https://app.example.test");
  const blocked = await fetch(endpoint + "/functions/session", { headers: { origin: "https://unlisted.example.test" } });
  assert.equal(blocked.status, 401);
  assert.equal(blocked.headers.has("access-control-allow-origin"), false);

  await writeFile(credentials, JSON.stringify({ alice: false, bob: true }));
  await db!.insert("tasks", { title: "After revocation", owner: "alice" });
  await until(() => states.at(-1)?.status === "error");
  assert.equal(states.at(-1)?.data, undefined);
  stop!(); stop = undefined;
  bearer = "bob"; browser.setAuth({ key: "bob" });
  assert.deepEqual(await calls.list(), []);
  assert.equal((await calls.create()).owner, "bob");
  assert.deepEqual({ ...await calls.validated({ title: "  portable validators  " }) }, { label: "PORTABLE VALIDATORS" });
  assert.deepEqual({ ...await calls.valibot({ title: "  PORTABLE QUERY  " }) }, { label: "portable query" });
  await db!.insert('Folders', { slug: 'guides' });
  await db!.insert('bookmarks', { folder: 'guides', owner: 'bob' });
  const relatedRow = await calls.createRelation({ folder: 'guides', title: 'Guide', low: 1, high: 2 });
  assert.equal(relatedRow.state, 'draft');
  await assert.rejects(Promise.resolve(calls.createRelation({ folder: 'guides', title: 'x', low: 1, high: 2 })));
  await assert.rejects(Promise.resolve(calls.createRelation({ folder: 'guides', title: 'Bad range', low: 3, high: 2 })));
  let relatedState: { relations: unknown[]; bookmarks: { folder: string | null }[] } | undefined;
  const stopRelated = calls.related.subscribe((state: { data?: typeof relatedState }) => { relatedState = state.data; });
  try {
    await until(() => relatedState?.relations.length === 1 && relatedState.bookmarks[0]?.folder === 'guides');
    // Only parent rows were explicitly written; both dependent child views must
    // refresh coherently for database-enforced cascade and set-null effects.
    await db!.delete('Folders').where({ slug: 'guides' });
    await until(() => relatedState?.relations.length === 0 && relatedState.bookmarks[0]?.folder === null);
  } finally { stopRelated(); }
  assert.deepEqual({ ...await calls.visit({ key: 'guide', fail: false }) }, { key: 'guide', visits: 1n });
  assert.deepEqual({ ...await calls.visit({ key: 'guide', fail: false }) }, { key: 'guide', visits: 2n });
  assert.equal(await calls.skipVisit(), null);
  await assert.rejects(Promise.resolve(calls.visit({ key: 'guide', fail: true })), { code: 'row_expectation_failed' });
  await assert.rejects(Promise.resolve(calls.caughtCount()), { code: 'row_expectation_failed' });
  const counters = await db!.select('counters').where({ owner: 'bob' });
  assert.equal(counters.length, 1);
  assert.equal((counters[0] as { visits: bigint }).visits, 2n);
  const event = await calls.createEvent({ when: new Date('2026-10-20T12:34:56.789Z') });
  assert.ok(event.when instanceof Date);
  assert.equal(event.when.toISOString(), '2026-10-20T12:34:56.789Z');
  assert.equal(event.day, '2026-10-20');
  assert.equal(event.precise, '2026-10-20T12:34:56.123456Z');
  assert.equal(event.safe, 42);
  assert.equal(event.amount, 10);
  const eventRows = await calls.eventDates();
  assert.equal(eventRows[0].at.toISOString(), '2026-10-20T12:34:56.789Z');
  eventRows[0].at.setTime(0);
  assert.equal((await calls.eventDates())[0].at.toISOString(), '2026-10-20T12:34:56.789Z');
  await assert.rejects(Promise.resolve(calls.limitInput('x'.repeat(4096))), { code: 'execution_limit' });
  await assert.rejects(Promise.resolve(calls.limitOutput()), { code: 'execution_limit' });
  await assert.rejects(Promise.resolve(calls.limitReads()), { code: 'execution_limit' });
  await assert.rejects(Promise.resolve(calls.limitOperations()), { code: 'execution_limit' });
  await assert.rejects(Promise.resolve(calls.limitExecution()), { code: 'execution_limit' });
  assert.equal(await calls.raisedLimits('x'.repeat(3000)), 'x'.repeat(6000));
  assert.deepEqual(await db!.select('tasks').where('title', 'startsWith', 'limit:'), []);
  // Exercise a real raised input/output ceiling across HTTP, native IPC and the
  // restricted worker: no fixed 16 MiB framing cap may override this declaration.
  const large = 'x'.repeat(17 * 1024 * 1024);
  assert.equal((await calls.largePayload(large)).length, large.length);
  await db!.insert('metrics', [{ owner: 'bob', amount: 9223372036854775807n }, { owner: 'bob', amount: 9223372036854775807n }, { owner: 'alice', amount: 1n }]);
  assert.deepEqual({ ...await calls.metricsSummary() }, { count: 2, total: 18446744073709551614n });
  const groups = await calls.metricGroups();
  assert.deepEqual(groups.map((row: object) => ({ ...row })), [{ owner: 'bob', count: 2, total: 18446744073709551614n }]);
  let total: bigint | null | undefined;
  stopSummary = calls.metricsSummary.subscribe((state: { data?: { total: bigint | null } }) => { total = state.data?.total; });
  await until(() => total === 18446744073709551614n);
  await db!.insert('metrics', { owner: 'bob', amount: 1n });
  await until(() => total === 18446744073709551615n);
  stopSummary!(); stopSummary = undefined;
  const payload = { tags: ["blue"], nested: { score: 1.25, empty: null } };
  const jsonRow = await calls.jsonWrite({ payload, score: 1e20 });
  assert.deepEqual(JSON.parse(JSON.stringify(jsonRow.payload)), payload);
  assert.equal(jsonRow.score, 1e20);
  assert.deepEqual(Object.keys(jsonRow).sort(), ["payload", "score"]);
  const jsonRows = await calls.jsonList();
  assert.equal(jsonRows.length, 1);
  assert.deepEqual(JSON.parse(JSON.stringify(jsonRows[0].payload)), payload);
  const beforeInvalid = await calls.list();
  const expectedFailure = { code: 'unauthorized', message: 'This task is locked', details: { taskId: 'locked', reasons: [null, 'archived'] } };
  await assert.rejects(Promise.resolve(calls.intentionalFailure()), expectedFailure);
  assert.equal(await calls.who(), 'bob'); // Application codes must not retire auth.
  await assert.rejects(Promise.resolve(calls.failedQuery()), { code: 'capacity', message: 'This query is disabled', details: null });
  await assert.rejects(Promise.resolve(calls.unexpected()), error => error instanceof Error && !JSON.stringify(error).includes('PRIVATE_DRIVER'));
  await assert.rejects(Promise.resolve(calls.oversizedError()), { code: 'execution_limit' });
  const failedStates: { status: string; error?: unknown }[] = [];
  const stopFailure = calls.failedQuery.subscribe((state: typeof failedStates[number]) => failedStates.push(state));
  try {
    await until(() => failedStates.some(state => state.status === 'error'));
    assert.deepEqual(JSON.parse(JSON.stringify(failedStates.find(state => state.status === 'error')!.error)), { code: 'capacity', message: 'This query is disabled', details: null });
  } finally { stopFailure(); }
  // Recover the exact public failure after a lost response, without executing again.
  const headers = { authorization: 'Bearer bob', 'content-type': 'application/json' };
  const session = await (await fetch(endpoint + '/functions/session', { headers })).json() as { source: string; ticket: { id: string } };
  const requestId = randomUUID();
  const rejected = await fetch(endpoint + '/functions/mutate', { method: 'POST', headers, body: JSON.stringify({ name: 'tasks.intentionalFailure', input: null, request_id: requestId, ticket: session.ticket.id, expected_source: session.source }) });
  assert.equal(rejected.status, 422);
  assert.deepEqual(await rejected.json(), { code: 'execution_failed', application: expectedFailure });
  const recovered = await fetch(endpoint + '/functions/receipts/' + requestId + '?source=' + session.source + '&ticket=' + session.ticket.id, { headers });
  assert.equal(recovered.status, 422);
  assert.deepEqual(await recovered.json(), { code: 'execution_failed', application: expectedFailure });
  assert.deepEqual(await calls.list(), beforeInvalid);
  await assert.rejects(Promise.resolve(calls.validated({ title: 7 })));
  // The insert succeeds before output validation fails; the transaction must roll back.
  await assert.rejects(Promise.resolve(calls.validated({ title: "   " })));
  assert.deepEqual(await calls.list(), beforeInvalid);
  // These rows span loaded and unloaded ranges. Changes before an old page end
  // must not refill that page and duplicate rows from its successor.
  const seeded = await db!.insert('tasks', ['a', 'b', 'c', 'd', 'e'].map(title => ({ title: 'page:' + title, owner: 'bob' })));
  const pages = calls.paged.paginate((cursor: string | null) => ({ limit: 2, cursor }));
  disposePages = () => pages.dispose();
  const pageStates: { status: string; data?: { id: string; title: string }[] }[] = [];
  const stopPages = pages.subscribe((state: typeof pageStates[number]) => pageStates.push(state));
  const titles = () => pageStates.at(-1)?.data?.map(row => row.title).join(',');
  await until(() => titles() === 'page:a,page:b');
  await db!.insert('tasks', { title: 'page:aa', owner: 'bob' });
  await until(() => titles() === 'page:a,page:aa,page:b');
  await pages.next();
  await until(() => titles() === 'page:a,page:aa,page:b,page:c,page:d');
  await db!.delete('tasks').where({ id: seeded.find((row: { title: string }) => row.title === 'page:b')!.id });
  await until(() => titles() === 'page:a,page:aa,page:c,page:d');
  await db!.update('tasks', { title: 'page:z' }).where({ id: seeded.find((row: { title: string }) => row.title === 'page:a')!.id });
  await until(() => titles() === 'page:aa,page:c,page:d');
  await pages.next(2);
  await until(() => titles() === 'page:aa,page:c,page:d,page:e,page:z');
  assert.equal(pages.hasMore, false);
  for (const state of pageStates) {
    if (state.data) {
      assert.equal(new Set(state.data.map(row => row.id)).size, state.data.length, 'loaded ranges must never publish duplicate rows');
      for (const row of state.data) assert.deepEqual(Object.keys(row).sort(), ['id', 'title'], 'live refreshes must preserve the selected fields');
    }
  }
  browser.setAuth(null);
  await until(() => pageStates.at(-1)?.status === 'error');
  assert.equal(pageStates.at(-1)?.data, undefined, 'retired auth clears all loaded pages');
  stopPages(); pages.dispose(); disposePages = undefined;
  assert.equal(output.includes(url), false);
  assert.deepEqual(await digestGenerated(generated), generatedArtifacts, "serving must not regenerate or change artifacts");
  await dispose!(); dispose = undefined;
  await db!.close(); db = undefined;
  service.kill("SIGTERM");
  const shutdown = await Promise.race([serviceExit, delay(5000, "timeout", { ref: false })]);
  assert.equal(shutdown, 0, "CLI must shut down cleanly");
  console.log("PASS: generated CLI service, automatic browser setup, verified identity, live direct writes, revocation and shutdown");
} finally {
  if (originalLocation) Object.defineProperty(globalThis, "location", originalLocation);
  else Reflect.deleteProperty(globalThis, "location");
  stopSummary?.(); disposePages?.(); stop?.(); await dispose?.(); await db?.close();
  service?.kill("SIGKILL"); await serviceExit;
  for (const directory of frozenDirectories) await chmod(directory, 0o755);
  await rm(root, { recursive: true, force: true });
}
