import { initializeDatabase, connectDatabase } from "./fixtures/sqlite.ts";
import { expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { defineSchema, defineTable, v } from "../src/index.ts";
import { generateDatabase } from "../src/codegen.ts";
import type { primary } from "./fixtures/database-server.ts";
import type { DatabaseSource } from "../src/database-configuration.ts";
import type * as definitions from "./fixtures/database-functions.ts";
import type { MutationCallable, QueryCallable } from "../src/callable.ts";
import type { OfflineResolver } from "../src/offline.ts";
import type { QueryState } from "../src/contracts.ts";
import { createQueryObserver } from "../src/bindings.ts";

const id = "00000000-0000-0000-0000-000000000005";
const original = { id, workspace: "alice", title: "Original", version: "0" };
const edited = { ...original, title: "Canonical", version: "1" };
const head = { source: "00000000-0000-0000-0000-000000000001", scope: "00000000-0000-0000-0000-000000000002", principal: "00000000-0000-0000-0000-000000000003", authorization_scope: "00000000-0000-0000-0000-000000000004", revision: "1" };
const decisions: string[] = [];
const resolveConflict: OfflineResolver<{ id: string; title: string; version: bigint }> = ({ input, error }) => {
  decisions.push(error.code);
  if (error.code === "stale_task") return { action: "retry", input: { ...input, version: 1n } };
  if (error.code === "needs_review") return { action: "defer" };
  return { action: "reject" };
};

/** Scripted transport responses exercise client storage/recovery, not server business rules. */
function transport() {
  let online = true, receiptOnline = true, drop = false;
  let failure: string | undefined;
  const posts: { request_id: string; ticket: string; input: { version: string; title: string } }[] = [];
  const lookups: string[] = [];
  const receipts = new Map<string, unknown>();
  let rows = [original], revision = "1";
  const streams = new Set<{ controller: ReadableStreamDefaultController<Uint8Array>; id: string; next: number; queries: { key: string }[] }>();
  const emit = (stream: typeof streams extends Set<infer T> ? T : never) => {
    const next = ++stream.next;
    stream.controller.enqueue(new TextEncoder().encode(`event: round\nid: ${stream.id}:${next}\ndata: ${JSON.stringify({ subscription: stream.id, sequence: String(next), round: { ...head, revision, results: stream.queries.map(query => ({ key: query.key, result: rows })) } })}\n\n`));
  };
  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "content-type": "application/json" } });
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    const url = new URL(request.url);
    if (url.pathname === "/functions/session") return json({ ...head, ticket: { id: crypto.randomUUID(), expires_in_ms: 60000, renew_after_ms: 30000 } });
    if (url.pathname === "/functions/query") return json({ ...head, revision, result: rows });
    if (url.pathname === "/functions/subscriptions") {
      const body = await request.json() as { queries: { key: string }[] };
      return new Response(new ReadableStream<Uint8Array>({ start(controller) {
        const stream = { controller, id: crypto.randomUUID(), next: 0, queries: body.queries };
        streams.add(stream); emit(stream);
        request.signal.addEventListener("abort", () => { streams.delete(stream); try { controller.close(); } catch {} }, { once: true });
      } }), { headers: { "content-type": "text/event-stream" } });
    }
    if (url.pathname.endsWith("/ack")) return json(null);
    if (url.pathname.startsWith("/functions/receipts/")) {
      lookups.push(url.searchParams.get("ticket")!);
      return json(receipts.get(url.pathname.split("/").at(-1)!) ?? null);
    }
    if (url.pathname === "/functions/mutate") {
      const body = await request.json() as typeof posts[number];
      posts.push(body);
      if (failure) { const code = failure; failure = undefined; return json({ code: "execution_failed", application: { code, message: "Scripted conflict" } }, 422); }
      const receipt = { request_id: body.request_id, source: head.source, scope: head.scope, revision: "2", result: [edited] };
      receipts.set(body.request_id, receipt);
      return json(receipt);
    }
    return json({ code: "not_found" }, 404);
  } });
  const fetcher = (async (input: string | URL | Request, options?: RequestInit) => {
    const url = new URL(String(input));
    if (!online || (!receiptOnline && url.pathname.startsWith("/functions/receipts/") && !url.pathname.endsWith("/ack"))) throw new Error("Injected network outage");
    const response = await fetch(input, options);
    if (drop && url.pathname === "/functions/mutate" && response.ok) { drop = false; await response.arrayBuffer(); throw new Error("Injected acknowledgement loss"); }
    return response;
  }) as typeof fetch;
  return { url: server.url.href, fetch: fetcher, posts, lookups,
    online(value: boolean) { online = value; if (!value) { for (const stream of streams) try { stream.controller.close(); } catch {} streams.clear(); } }, receiptsOnline(value: boolean) { receiptOnline = value; },
    queryEdited() { rows = [edited]; revision = "2"; },
    publishEdited() { rows = [edited]; revision = "2"; for (const stream of streams) emit(stream); },
    dropAcknowledgement() { drop = true; }, failNext(code: string) { failure = code; },
    close: () => server.stop(true),
  };
}
async function until(check: () => boolean | Promise<boolean>) {
  const deadline = Date.now() + 4000;
  while (!await check()) { if (Date.now() > deadline) throw new Error("Client condition timed out"); await Bun.sleep(5); }
}
async function fixture() {
  const directory = await mkdtemp(new URL("./.offline-replica-", import.meta.url).pathname);
  let peer: ReturnType<typeof transport> | undefined;
  let close: (() => Promise<void>) | undefined;
  try {
    const serverSource = new URL("./fixtures/database-server.ts", import.meta.url).pathname;
    const functionSource = new URL("./fixtures/database-functions.ts", import.meta.url).pathname;
    await Bun.write(join(directory, "server.ts"), `export { primary } from ${JSON.stringify(serverSource)}\n`);
    await Bun.write(join(directory, "tasks.ts"), `export { byWorkspace, rename } from ${JSON.stringify(functionSource)}\n`);
    await generateDatabase(join(directory, "server.ts"));
    const functions = await import(Bun.pathToFileURL(join(directory, "_generated/server/tasks.ts")).href) as {
      byWorkspace: QueryCallable<typeof definitions.byWorkspace>;
      rename: MutationCallable<typeof definitions.rename>;
    };
    const { source } = await import(Bun.pathToFileURL(join(directory, "_generated/server/source.ts")).href) as { source: DatabaseSource<typeof primary.schema> };
    const device = defineSchema({ tasks: source.tables.tasks.omit("internalNotes").replica(), drafts: defineTable({ title: v.string() }) });
    peer = transport();
    const settings = { filename: join(directory, "device.sqlite"), sources: [{ database: source, url: peer.url, auth: { key: "alice" }, fetch: peer.fetch, reconnectDelayMs: 5, operationTimeoutMs: 100, confirmationTimeoutMs: 200 }] };
    const local = await initializeDatabase(device, settings);
    close = local.close;
    const bound = local.bind(functions);
    return { directory, peer, device, settings, local, source, functions: bound, references: functions, queued: bound.rename.offline(resolveConflict) };
  } catch (error) {
    await close?.();
    await peer?.close();
    await rm(directory, { recursive: true, force: true });
    throw error;
  }
}

test("instance-bound generated callers persists authorized rows and queues calls until fresh connectivity is verified", async () => {
  const f = await fixture();
  try {
    expect(await f.functions.byWorkspace({ workspace: "alice" })).toEqual([{ ...original, version: 0n }]);
    expect(await f.local.select("tasks")).toEqual([{ ...original, version: 0n }]);
    f.peer.online(false);
    const cached = f.functions.byWorkspace({ workspace: "alice" });
    expect(await cached).toEqual([{ ...original, version: 0n }]);
    expect(cached.isStale).toBe(true);
    const call = f.queued({ id, title: "Queued", version: 0n });
    await until(async () => (await f.local.pendingMutations()).length === 1);
    expect((await f.local.pendingMutations())[0]!.state).toBe("queued");
    expect(f.peer.posts).toHaveLength(0);
    f.peer.online(true);
    await f.local.flush();
    expect(await call).toEqual([{ ...edited, version: 1n }]);
    expect(f.peer.posts).toHaveLength(1);
    const committed = (await f.local.pendingMutations())[0]!;
    expect(committed.state).toBe("committed");
    await f.local.acknowledgeMutation(committed);
    expect(await f.local.pendingMutations()).toEqual([]);
  } finally { await f.local.close(); await f.peer.close(); await rm(f.directory, { recursive: true, force: true }); }
});

test("a lost acknowledgement survives close/reopen and recovers the same ticket without resubmission", async () => {
  const f = await fixture();
  let local = f.local;
  try {
    await f.functions.byWorkspace({ workspace: "alice" });
    f.peer.dropAcknowledgement(); f.peer.receiptsOnline(false);
    const call = f.queued({ id, title: "Queued", version: 0n });
    const settled = Promise.resolve(call).catch(error => error);
    await until(() => f.peer.posts.length === 1);
    await local.close();
    expect(await settled).toMatchObject({ code: "context_changed" });
    f.peer.receiptsOnline(true);
    local = await connectDatabase(f.device, f.settings);
    local.bind(f.references.rename).offline(resolveConflict);
    await local.flush();
    const [pending] = await local.pendingMutations();
    expect(pending!.state).toBe("committed");
    expect(pending!.ticket).toBe(f.peer.posts[0]!.ticket);
    expect(f.peer.lookups).toContain(f.peer.posts[0]!.ticket);
    expect(f.peer.posts).toHaveLength(1);
    await local.acknowledgeMutation(pending!);
  } finally { await local.close(); await f.peer.close(); await rm(f.directory, { recursive: true, force: true }); }
});

test("queued calls can show optimistic feedback without modifying replica rows locally", async () => {
  const f = await fixture();
  const states: QueryState<Awaited<ReturnType<typeof f.functions.byWorkspace>>>[] = [];
  const stop = f.functions.byWorkspace.subscribe({ workspace: "alice" }, state => states.push(state));
  try {
    await until(() => states.at(-1)?.status === "success");
    // A newer one-shot read cannot advance persisted membership ahead of its live round.
    f.peer.queryEdited();
    expect(await f.functions.byWorkspace({ workspace: "alice" })).toEqual([{ ...edited, version: 1n }]);
    expect((await f.local.select("tasks"))[0]!.title).toBe("Original");
    const rename = f.queued.optimistic({ query: f.functions.byWorkspace, input: () => ({ workspace: "alice" }), update: (rows, input) => rows.map(row => row.id === input.id ? { ...row, title: input.title } : row) });
    f.peer.online(false);
    const call = rename({ id, title: "Queued", version: 0n });
    await until(() => states.some(state => state.status === "success" && state.isOptimistic && state.data[0]?.title === "Queued"));
    expect((await f.local.select("tasks"))[0]!.title).toBe("Original");
    f.peer.online(true);
    await f.local.flush(); await call;
    f.peer.publishEdited();
    await until(() => { const state = states.at(-1); return state?.status === "success" && !state.isOptimistic && state.data[0]?.title === "Canonical"; });
    expect((await f.local.select("tasks"))[0]!.title).toBe("Canonical");
  } finally { stop(); await f.local.close(); await f.peer.close(); await rm(f.directory, { recursive: true, force: true }); }
});

test("device conflict callbacks explicitly retry with new input or defer while preserving the intent", async () => {
  const f = await fixture();
  try {
    await f.functions.byWorkspace({ workspace: "alice" });
    f.peer.failNext("stale_task");
    expect(await f.queued({ id, title: "Queued", version: 0n })).toEqual([{ ...edited, version: 1n }]);
    expect(f.peer.posts.map(post => post.input.version)).toEqual(["0", "1"]);
    expect(f.peer.posts[1]!.request_id).not.toBe(f.peer.posts[0]!.request_id);
    await f.local.acknowledgeMutation((await f.local.pendingMutations())[0]!);
    f.peer.failNext("needs_review");
    await expect(Promise.resolve(f.queued({ id, title: "Deferred", version: 0n }))).rejects.toMatchObject({ code: "offline_conflict" });
    const deferred = (await f.local.pendingMutations())[0]!;
    expect(deferred.state).toBe("conflict");
    await f.local.resolveConflict(deferred, { action: "reject" });
    await f.local.acknowledgeMutation(deferred);
    expect(await f.local.pendingMutations()).toEqual([]);
    expect(decisions).toContain("stale_task"); expect(decisions).toContain("needs_review");
  } finally { await f.local.close(); await f.peer.close(); await rm(f.directory, { recursive: true, force: true }); }
});

test("instance bindings resume after promised auth updates and remain closed after another instance opens", async () => {
  const f = await fixture();
  let local = f.local;
  const observer = createQueryObserver(f.functions.byWorkspace, { workspace: "alice" });
  const stop = observer.subscribe(() => {});
  try {
    await until(() => observer.getSnapshot().status === "success");
    let resolve!: (value: { key: string }) => void;
    const loading = local.setAuth(f.source, new Promise<{ key: string }>(value => { resolve = value; }));
    expect(observer.getSnapshot().status).toBe("pending");
    await expect(Promise.resolve(local.select("tasks"))).rejects.toMatchObject({ code: "replica_scope_uninitialized" });
    resolve({ key: "alice" });
    await loading;
    await until(() => observer.getSnapshot().status === "success");
    expect((await local.select("tasks"))[0]!.title).toBe("Original");
    await local.close();
    expect(observer.getSnapshot().status).toBe("error");
    local = await connectDatabase(f.device, f.settings);
    expect(observer.getSnapshot().status).toBe("error");
    const rebound = local.bind(f.references);
    expect(await rebound.byWorkspace({ workspace: "alice" })).toEqual([{ ...original, version: 0n }]);
    expect((await local.select("tasks"))[0]!.title).toBe("Original");
  } finally { stop(); await local.close(); await f.peer.close(); await rm(f.directory, { recursive: true, force: true }); }
});

test('two device instances bind the same source without sharing caches, policies or lifetime', async () => {
  const f = await fixture();
  const peer = transport();
  const second = await initializeDatabase(f.device, { ...f.settings, filename: join(f.directory, 'second.sqlite'), sources: [{ ...f.settings.sources[0]!, url: peer.url, fetch: peer.fetch }] });
  const functions = second.bind(f.references);
  try {
    // Independent policy choices for the same operation are valid across apps.
    f.local.bind(f.references.rename.offline(resolveConflict));
    second.bind(f.references.rename.offline(() => ({ action: 'reject' })));
    expect(functions.byWorkspace).not.toBe(f.functions.byWorkspace);
    expect(await f.functions.byWorkspace({ workspace: 'alice' })).toEqual([{ ...original, version: 0n }]);
    peer.queryEdited();
    expect(await functions.byWorkspace({ workspace: 'alice' })).toEqual([{ ...edited, version: 1n }]);
    expect((await f.local.select('tasks'))[0]!.title).toBe('Original');
    expect((await second.select('tasks'))[0]!.title).toBe('Canonical');
    await f.local.close();
    await expect(f.local.pendingMutations()).rejects.toMatchObject({ code: 'disposed' });
    expect(await functions.byWorkspace({ workspace: 'alice' })).toEqual([{ ...edited, version: 1n }]);
  } finally { await Promise.all([f.local.close(), second.close()]); await peer.close(); await f.peer.close(); await rm(f.directory, { recursive: true, force: true }); }
});

test('local function views react to replica downloads and retire immediately during account changes', async () => {
  const f = await fixture();
  const rows = f.device.query(({ db }) => db.select('tasks'));
  const { sqlite } = await import('@hulla/db-sqlite');
  const db = sqlite.initialize({ schema: f.device, filename: join(f.directory, 'local-views.sqlite'), sources: f.settings.sources, functions: async () => ({ rows }) });
  await db.ready();
  const states: unknown[] = [];
  const stop = db.functions.rows.subscribe(state => { if (state.status === 'success') states.push(state.data); });
  let release!: (value: { key: string }) => void;
  try {
    await db.bind(f.references.byWorkspace)({ workspace: 'alice' });
    await until(() => states.some(value => Array.isArray(value) && value.length === 1));
    const settled = db.functions.rows();
    expect(await settled).toEqual([{ ...original, version: 0n }]);
    const update = db.setAuth(f.source, new Promise<{ key: string }>(resolve => { release = resolve; }));
    expect(settled.status).toBe('error');
    expect(settled.data).toBeUndefined();
    release({ key: 'alice' });
    await update;
    expect(await db.functions.rows()).toEqual([{ ...original, version: 0n }]);
  } finally { release?.({ key: 'alice' }); stop(); await Promise.all([db.close(), f.local.close()]); await f.peer.close(); await rm(f.directory, { recursive: true, force: true }); }
});
