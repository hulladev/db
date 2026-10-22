import { afterEach, expect, spyOn, test } from "bun:test";
import { HttpRuntime, type HttpRuntimeOptions } from "../src/http.ts";
import { DatabaseError, type ClientAuthState, type QueryState, type MutationState } from "../src/index.ts";
import { valueCodec } from "../src/codec.ts";
import { queryBinding } from "../src/callable.ts";
import { createRuntimeScope } from "../src/runtime.ts";
import { sseEvents } from "../src/http/sse.ts";
import { runtime, contracts } from "./generated/runtime.ts";
import { create, pending } from "./generated/notes.ts";

type FixtureSession = { scope: string; principal: string; authorizationScope: string; bearer: string };
const scope = "00000000-0000-0000-0000-000000000001", source = "00000000-0000-0000-0000-000000000002";
const alice: FixtureSession = { scope, principal: "00000000-0000-0000-0000-000000000003", authorizationScope: "00000000-0000-0000-0000-000000000004", bearer: "alice" };
const bob: FixtureSession = { scope, principal: "00000000-0000-0000-0000-000000000005", authorizationScope: "00000000-0000-0000-0000-000000000006", bearer: "bob" };
const anonymous: FixtureSession = { scope, principal: "00000000-0000-0000-0000-000000000000", authorizationScope: "00000000-0000-0000-0000-000000000008", bearer: "anonymous" };
const baseUrl = "http://127.0.0.1:9000";
const cleanups: (() => void | Promise<void>)[] = [];
afterEach(async () => { for (const cleanup of cleanups.splice(0).reverse()) await cleanup(); });
const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "content-type": "application/json" } });
async function until(condition: () => boolean): Promise<void> {
  const deadline = Date.now() + 2000;
  while (!condition()) { if (Date.now() > deadline) throw new Error("condition timed out"); await new Promise(r => setTimeout(r, 1)); }
}
/** Stateful peer for deterministic failure injection; PostgreSQL integration runs separately. */
function peer() {
  let revision = 0, opens = 0, online = true, lostAcknowledgements = 0, recoveryOffline = false, denied = false, expired = false, currentSource = source;
  const rows: { id: number; title: string; owner: string }[] = [], submissions: string[] = [];
  const receipts = new Map<string, unknown>();
  const gates = new Map<string, { promise: Promise<void>; release: () => void; rejected: boolean }>();
  const acknowledgements = new Map<string, { promise: Promise<void>; release: () => void }>();
  let canonicalize = false, lookups = 0;
  const streams = new Set<{ controller: ReadableStreamDefaultController<Uint8Array>; session: FixtureSession; id: string; next: number; queries: { key: string }[] }>();
  const snapshot = (session: FixtureSession) => ({ source: currentSource, scope, principal: session.principal, authorization_scope: session.authorizationScope, revision: String(revision), result: rows.filter(row => row.owner === session.bearer) });
  const emit = (stream: typeof streams extends Set<infer T> ? T : never, result?: unknown) => {
    const { result: data, ...head } = (result ?? snapshot(stream.session)) as ReturnType<typeof snapshot>;
    const next = ++stream.next, frame = { subscription: stream.id, sequence: String(next), round: { ...head, results: stream.queries.map(query => ({ key: query.key, result: data })) } };
    stream.controller.enqueue(new TextEncoder().encode(`event: round\nid: ${stream.id}:${next}\ndata: ${JSON.stringify(frame)}\n\n`));
  };
  const fetcher = (async (urlValue: string | URL | Request, init?: RequestInit) => {
    init?.signal?.throwIfAborted(); if (!online) throw new Error("offline");
    const url = new URL(String(urlValue)), headers = new Headers(init?.headers);
    const bearer = headers.get("authorization");
    if (bearer && !["Bearer alice", "Bearer alice-refreshed", "Bearer bob"].includes(bearer)) return json({ code: "unauthorized" }, 401);
    const session = bearer?.startsWith("Bearer alice") ? alice : bearer === "Bearer bob" ? bob : anonymous;
    for (const [name, expected] of [["x-hulladb-scope", session.scope], ["x-hulladb-principal", session.principal], ["x-hulladb-authorization-scope", session.authorizationScope]]) {
      if (url.pathname !== "/functions/session") expect(headers.has(name!)).toBe(true);
      if (headers.has(name!) && headers.get(name!) !== expected) return json({ code: "unauthorized" }, 401);
    }
    if (denied) return json({ code: "forbidden" }, 403);
    if (url.pathname === "/functions/session") { const { result: _, ...head } = snapshot(session); return json({ ...head, ticket: { id: crypto.randomUUID(), expires_in_ms: 60_000, renew_after_ms: 30_000 } }); }
    if (url.pathname.endsWith("/ack")) { receipts.delete(`${session.principal}:${url.pathname.split("/").at(-2)!}`); return json(null); }
    if (url.pathname.startsWith("/functions/receipts/")) {
      lookups++;
      if (recoveryOffline) throw new Error("recovery offline");
      if (url.searchParams.get("source") !== currentSource) return json({ code: "reset_required" }, 409);
      const receipt = receipts.get(`${session.principal}:${url.pathname.split("/").at(-1)!}`);
      if (expired && receipt) return json({ code: "receipt_expired" }, 410);
      return json(receipt ?? null);
    }
    const body = JSON.parse(String(init?.body)) as { expected_source: string; request_id: string; input: { title: string }; queries: { key: string }[] };
    if (body.expected_source !== currentSource) return json({ code: "reset_required" }, 409);
    if (url.pathname === "/functions/query") return json(snapshot(session));
    if (url.pathname === "/functions/mutate") {
      submissions.push(body.request_id);
      const gate = gates.get(body.input.title);
      if (gate) { await gate.promise; if (gate.rejected) return json({ code: "conflict" }, 409); }
      const key = `${session.principal}:${body.request_id}`;
      let receipt = receipts.get(key);
      if (!receipt) {
        const row = { id: rows.length + 1, title: canonicalize ? body.input.title.trim().toUpperCase() : body.input.title, owner: session.bearer };
        rows.push(row); revision++;
        receipt = { request_id: body.request_id, source: currentSource, scope, revision: String(revision), result: row };
        receipts.set(key, receipt); for (const stream of streams) emit(stream);
      }
      const acknowledgement = acknowledgements.get(body.input.title);
      if (acknowledgement) await acknowledgement.promise;
      if (lostAcknowledgements-- > 0) throw new Error("response lost after durable commit");
      return json(receipt);
    }
    if (url.pathname === "/functions/subscriptions") {
      opens++;
      return new Response(new ReadableStream<Uint8Array>({ start(controller) {
        const stream = { controller, session, id: crypto.randomUUID(), next: 0, queries: body.queries }; streams.add(stream); emit(stream);
        init?.signal?.addEventListener("abort", () => { streams.delete(stream); try { controller.close(); } catch {} }, { once: true });
      } }), { headers: { "content-type": "text/event-stream" } });
    }
    return json({ code: "invalid_request" }, 400);
  }) as typeof fetch;
  return {
    hold(title: string) {
      let release!: () => void;
      const gate = { promise: new Promise<void>(resolve => { release = resolve; }), release: () => release(), rejected: false };
      gates.set(title, gate);
      return { release: gate.release, reject: () => { gate.rejected = true; gate.release(); } };
    },
    holdAcknowledgement(title: string) {
      let release!: () => void;
      const gate = { promise: new Promise<void>(resolve => { release = resolve; }), release: () => release() };
      acknowledgements.set(title, gate); return gate.release;
    },
    roundResults(transform: (results: { key: string; result: unknown }[]) => unknown[]) {
      for (const stream of streams) {
        const { result, ...head } = snapshot(stream.session), next = ++stream.next;
        const frame = { subscription: stream.id, sequence: String(next), round: { ...head, results: transform(stream.queries.map(query => ({ key: query.key, result }))) } };
        stream.controller.enqueue(new TextEncoder().encode(`event: round\nid: ${stream.id}:${next}\ndata: ${JSON.stringify(frame)}\n\n`));
      }
    },
    revision(value: number) { revision = value; },
    canonicalize() { canonicalize = true; },
    fetch: fetcher, rows, submissions, get opens() { return opens; }, get lookups() { return lookups; }, get streams() { return streams.size; },
    expireReceipts() { expired = true; },
    loseAcknowledgement() { lostAcknowledgements = 1; }, recoveryOffline(value: boolean) { recoveryOffline = value; },
    disconnect() { for (const stream of streams) stream.controller.close(); streams.clear(); }, online(value: boolean) { online = value; },
    forbid() { denied = true; for (const stream of streams) stream.controller.enqueue(new TextEncoder().encode('event: error\ndata: {"code":"forbidden"}\n\n')); },
    expireStreamCredentials() { for (const stream of streams) stream.controller.enqueue(new TextEncoder().encode('event: error\ndata: {"code":"unauthorized"}\n\n')); },
    replaceSource() { currentSource = "00000000-0000-0000-0000-000000000007"; },
    corrupt() { for (const stream of streams) emit(stream, { ...snapshot(stream.session), result: [{ id: "wrong", title: "invalid", owner: "alice" }] }); },
  };
}
function setup(model = peer(), extra: Partial<HttpRuntimeOptions> = {}) {
  let bearer: string | null = "alice";
  const adapter = new HttpRuntime({ baseUrl, auth: { key: "alice" }, headers: () => bearer ? { authorization: `Bearer ${bearer}` } : {}, contracts, fetch: model.fetch, reconnectDelayMs: 2, operationTimeoutMs: 1000, confirmationTimeoutMs: 200, ...extra });
  const unconfigure = runtime.configure(() => adapter); cleanups.push(() => adapter.dispose(), unconfigure);
  return { adapter, model,
    authenticate(session: FixtureSession | null) {
      bearer = session?.bearer ?? null;
      runtime.setAuth(session ? { key: session.bearer } : null);
    },
  };
}

test("app-owned expiry retires private handles and live state before anonymous calls", async () => {
  const { adapter, model, authenticate } = setup();
  const saved = create({ title: "private" });
  await saved;
  const mutationStates: MutationState<Awaited<typeof saved>>[] = [];
  saved.subscribe(state => mutationStates.push(state));
  const oneShot = pending();
  await oneShot;
  const states: QueryState<unknown>[] = [];
  pending.subscribe(state => states.push(state), { keepPreviousDataOnError: true });
  await until(() => states.at(-1)?.status === "success");
  expect(readyRows(states.at(-1))).toHaveLength(1);

  // This notification comes from the app's auth listener, not a DB expiry timer.
  authenticate(null);
  expect(states.at(-1)).toMatchObject({ status: "error", error: { code: "context_changed" } });
  expect(states.at(-1)?.data).toBeUndefined();
  expect(mutationStates.at(-1)).toMatchObject({ status: "error", error: { code: "context_changed" } });
  expect(model.streams).toBe(0);
  expect(oneShot.data).toBeUndefined();
  await expect(Promise.resolve(saved)).rejects.toMatchObject({ code: "context_changed" });
  const epoch = adapter.identityEpoch;
  authenticate(null);
  expect(adapter.identityEpoch).toBe(epoch);
  expect(await pending()).toEqual([]);
  expect(model.rows).toHaveLength(1);
});

test("optional auth expiry clears observed completed results without an app notification", async () => {
  const { model, adapter } = setup();
  const saved = create({ title: "private until expiry" });
  await saved;
  const mutationStates: MutationState<Awaited<typeof saved>>[] = [];
  saved.subscribe(state => mutationStates.push(state));
  const queryStates: QueryState<unknown>[] = [];
  pending.subscribe(state => queryStates.push(state), { keepPreviousDataOnError: true });
  await until(() => queryStates.at(-1)?.status === "success");
  const epoch = adapter.identityEpoch;
  runtime.setAuth({ key: "alice", expiresAt: Date.now() + 30 });
  expect(adapter.identityEpoch).toBe(epoch);
  await until(() => mutationStates.at(-1)?.status === "error");
  expect(queryStates.at(-1)).toMatchObject({ status: "error", error: { code: "context_changed" } });
  expect(queryStates.at(-1)?.data).toBeUndefined();
  expect(mutationStates.at(-1)).not.toHaveProperty("data");
  expect(model.streams).toBe(0);
  await expect(Promise.resolve(saved)).rejects.toMatchObject({ code: "context_changed" });
  const expiredEpoch = adapter.identityEpoch;
  runtime.setAuth(null);
  expect(adapter.identityEpoch).toBe(expiredEpoch);
});

test("refreshing or omitting an auth deadline keeps valid results and cancels the old timer", async () => {
  const { adapter } = setup();
  await create({ title: "still authorized" });
  const query = pending();
  await query;
  const epoch = adapter.identityEpoch, now = Date.now();
  const clock = spyOn(Date, "now").mockReturnValue(now);
  try {
    runtime.setAuth({ key: "alice", expiresAt: now + 15 });
    // A deadline beyond the timer's signed-32-bit range must not expire immediately.
    runtime.setAuth({ key: "alice", expiresAt: now + 3_000_000_000 });
    clock.mockReturnValue(now + 20);
    await new Promise(resolve => setTimeout(resolve, 30));
    expect(query.data).toHaveLength(1);
    expect(adapter.identityEpoch).toBe(epoch);
    runtime.setAuth({ key: "alice", expiresAt: now + 35 });
    runtime.setAuth({ key: "alice" });
    clock.mockReturnValue(now + 3_000_000_001);
    await new Promise(resolve => setTimeout(resolve, 30));
    expect(query.data).toHaveLength(1);
    expect(adapter.identityEpoch).toBe(epoch);
    for (const expiresAt of [NaN, Infinity, -1, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
      expect(() => runtime.setAuth({ key: "alice", expiresAt })).toThrow("expiresAt");
    }
    expect(query.data).toHaveLength(1);
    runtime.setAuth({ key: "alice", expiresAt: 0 });
    expect(query.data).toBeUndefined();
  } finally { clock.mockRestore(); }
});

test("suspended timers cannot expose expired handles or accept late mutation replies", async () => {
  const { adapter, model } = setup();
  await create({ title: "private" });
  const query = pending();
  await query;
  const gate = model.hold("late expiry result");
  const mutation = create({ title: "late expiry result" });
  await until(() => model.submissions.length === 2);
  const now = Date.now(), clock = spyOn(Date, "now").mockReturnValue(now);
  try {
    runtime.setAuth({ key: "alice", expiresAt: now + 60_000 });
    clock.mockReturnValue(now + 60_001);
    // No timer turn has occurred: merely accessing the handle must fence it.
    expect(query.data).toBeUndefined();
    gate.release();
    await expect(Promise.resolve(mutation)).rejects.toMatchObject({ code: "context_changed" });
    expect(mutation.data).toBeUndefined();
    runtime.setAuth({ key: "alice", expiresAt: now + 120_000 });
    expect(query.data).toBeUndefined();
    await adapter.dispose();
    const epoch = adapter.identityEpoch;
    clock.mockReturnValue(now + 120_001);
    expect(adapter.identityEpoch).toBe(epoch);
  } finally { gate.release(); clock.mockRestore(); }
});

test("expiry during recovery clears optimism and stops lookup without resubmission", async () => {
  const model = peer(), gate = model.hold("late private write");
  const { authenticate } = setup(model, { operationTimeoutMs: 15, confirmationTimeoutMs: 500 });
  const states: QueryState<unknown>[] = [];
  pending.subscribe(state => states.push(state), { keepPreviousDataOnError: true });
  await until(() => states.at(-1)?.status === "success");
  const call = optimisticCreate({ title: "late private write" });
  await until(() => call.status === "recovering" && model.lookups > 0);
  authenticate(null);
  await expect(Promise.resolve(call)).rejects.toMatchObject({ code: "context_changed" });
  const lookups = model.lookups;
  gate.release();
  await until(() => model.rows.length === 1);
  await new Promise(resolve => setTimeout(resolve, 25));
  expect(model.lookups).toBe(lookups);
  expect(model.submissions).toHaveLength(1);
  expect(states.at(-1)?.data).toBeUndefined();
  expect(call.data).toBeUndefined();
  expect(await pending()).toEqual([]);
});

test("an auth change fences a pending credential resolver before it can send a request", async () => {
  let release!: (value: HeadersInit) => void;
  const credential = new Promise<HeadersInit>(resolve => { release = resolve; });
  let first = true, requested = 0, resolving = false;
  const model = peer();
  setup(model, {
    headers: () => { if (first) { first = false; resolving = true; return credential; } return {}; },
    fetch: (async (url, init) => { requested++; return model.fetch(url, init); }) as typeof fetch,
  });
  const old = pending();
  await until(() => resolving);
  runtime.setAuth(null);
  release({ authorization: "Bearer alice" });
  await expect(Promise.resolve(old)).rejects.toMatchObject({ code: "context_changed" });
  expect(requested).toBe(0);
  expect(await pending()).toEqual([]);
});

test("client cache keys cannot supply server identity and identical auth notifications keep watches", async () => {
  const { adapter, model } = setup(peer(), {
    auth: { key: "arbitrary-app-account-key" },
    headers: { authorization: "Bearer alice", "x-hulladb-principal": bob.principal },
  });
  expect((await create({ title: "verified by server" })).owner).toBe("alice");
  const states: QueryState<unknown>[] = [];
  pending.subscribe(state => states.push(state));
  await until(() => states.at(-1)?.status === "success");
  const epoch = adapter.identityEpoch, count = states.length;
  runtime.setAuth({ key: "arbitrary-app-account-key" });
  expect(adapter.identityEpoch).toBe(epoch);
  expect(states).toHaveLength(count);
  expect(model.opens).toBe(1);
  runtime.setAuth({ key: "another-tenant" });
  expect(states.at(-1)).toMatchObject({ status: "error", error: { code: "context_changed" } });
  expect(model.streams).toBe(0);
});

test("loading auth sends nothing and invalid credentials never fall back to anonymous", async () => {
  let requested = 0, bearer: string | null = "expired";
  const model = peer();
  setup(model, {
    auth: new Promise(() => {}),
    headers: () => bearer ? { authorization: `Bearer ${bearer}` } : {},
    fetch: (async (url, init) => { requested++; return model.fetch(url, init); }) as typeof fetch,
  });
  await expect(Promise.resolve(pending())).rejects.toMatchObject({ code: "authentication_pending" });
  expect(requested).toBe(0);
  runtime.setAuth({ key: "alice" });
  await expect(Promise.resolve(pending())).rejects.toMatchObject({ code: "context_changed" });
  expect(model.opens).toBe(0);
  bearer = null;
  runtime.setAuth(null);
  expect(await pending()).toEqual([]);
});

test("a promised auth update fences private results while loading and can be awaited", async () => {
  const { adapter, model } = setup();
  const saved = create({ title: "private" });
  await saved;
  const states: QueryState<unknown>[] = [];
  pending.subscribe(state => states.push(state), { keepPreviousDataOnError: true });
  await until(() => states.at(-1)?.status === "success");
  runtime.setAuth({ key: "alice", expiresAt: Date.now() + 15 });
  let resolve!: (value: ClientAuthState) => void;
  const ready = runtime.setAuth(new Promise<ClientAuthState>(done => { resolve = done; }));
  expect(states.at(-1)).toMatchObject({ status: "error", error: { code: "context_changed" } });
  expect(states.at(-1)?.data).toBeUndefined();
  expect(saved.data).toBeUndefined();
  expect(model.streams).toBe(0);
  // The previous expiry timer must not turn pending auth into anonymous access.
  await new Promise(done => setTimeout(done, 25));
  await expect(Promise.resolve(pending())).rejects.toMatchObject({ code: "authentication_pending" });
  const epoch = adapter.identityEpoch;
  resolve({ key: "alice" });
  await ready;
  expect(adapter.identityEpoch).toBeGreaterThan(epoch);
  expect(await pending()).toHaveLength(1);
  expect(saved.data).toBeUndefined();
});

test("only the latest auth update can resolve, including promise and synchronous replacements", async () => {
  let resolveFirst!: (value: ClientAuthState) => void;
  const first = new Promise<ClientAuthState>(resolve => { resolveFirst = resolve; });
  const { adapter } = setup(peer(), { auth: first });
  let resolveLatest!: (value: ClientAuthState) => void;
  const latest = runtime.setAuth(new Promise<ClientAuthState>(resolve => { resolveLatest = resolve; }));
  resolveLatest({ key: "alice" });
  await latest;
  const query = pending();
  await query;
  const epoch = adapter.identityEpoch;
  resolveFirst({ key: "stale-account" });
  await first; await Promise.resolve();
  expect(adapter.identityEpoch).toBe(epoch);
  expect(query.status).toBe("success");

  let rejectOld!: (error: Error) => void;
  const obsolete = runtime.setAuth(new Promise<ClientAuthState>((_, reject) => { rejectOld = reject; }));
  runtime.setAuth(null);
  const signedOutEpoch = adapter.identityEpoch;
  rejectOld(new Error("stale provider failure"));
  await obsolete;
  expect(adapter.identityEpoch).toBe(signedOutEpoch);
  // Explicit null still permits requests subject to independent server verification.
  expect(await pending()).toEqual([]);
});

test("failed or malformed promised auth blocks calls until an explicit new update", async () => {
  const model = peer();
  let requests = 0;
  const { adapter } = setup(model, {
    fetch: (async (url, init) => { requests++; return model.fetch(url, init); }) as typeof fetch,
  });
  const failure = new Error("provider failed");
  await expect(Promise.resolve(runtime.setAuth(Promise.reject(failure)))).rejects.toBe(failure);
  await expect(Promise.resolve(pending())).rejects.toMatchObject({ code: "authentication_failed" });
  expect(requests).toBe(0);
  await expect(Promise.resolve(runtime.setAuth(Promise.resolve({ key: "alice", status: "authenticated" } as unknown as ClientAuthState)))).rejects.toBeInstanceOf(TypeError);
  await expect(Promise.resolve(pending())).rejects.toMatchObject({ code: "authentication_failed" });
  expect(requests).toBe(0);
  await runtime.setAuth(Promise.resolve(null));
  expect(await pending()).toEqual([]);

  let resolve!: (state: ClientAuthState) => void;
  const afterDispose = runtime.setAuth(new Promise<ClientAuthState>(done => { resolve = done; }));
  await adapter.dispose();
  const epoch = adapter.identityEpoch;
  resolve({ key: "alice" });
  await afterDispose;
  expect(adapter.identityEpoch).toBe(epoch);
  await expect(Promise.resolve(pending())).rejects.toMatchObject({ code: "disposed" });
});

test("a reentrant auth listener can start a new scope without old cleanup deleting it", async () => {
  const { authenticate, model } = setup();
  const newStates: QueryState<unknown>[] = [];
  let changed = false;
  pending.subscribe(state => {
    if (state.status === "error" && !changed) {
      changed = true;
      authenticate(bob);
      pending.subscribe(next => newStates.push(next));
    }
  });
  await until(() => model.streams === 1);
  authenticate(null);
  await until(() => newStates.at(-1)?.status === "success");
  expect(model.streams).toBe(1);
  expect((await create({ title: "Bob's own task" })).owner).toBe("bob");
  await until(() => readyRows(newStates.at(-1)).length === 1);
});

test("provider-refreshed credentials reconnect a live query without changing its cache identity", async () => {
  let token = "alice";
  const { model, adapter } = setup(peer(), { headers: () => ({ authorization: `Bearer ${token}` }) });
  const states: QueryState<unknown>[] = [];
  pending.subscribe(state => states.push(state));
  await until(() => states.at(-1)?.status === "success");
  const epoch = adapter.identityEpoch;
  token = "alice-refreshed";
  runtime.setAuth({ key: "alice" });
  model.expireStreamCredentials();
  await until(() => model.opens === 2 && model.streams === 1);
  expect(adapter.identityEpoch).toBe(epoch);
  expect(states.some(state => state.status === "error")).toBe(false);
  await create({ title: "after provider refresh" });
  await until(() => readyRows(states.at(-1)).length === 1);
});

test("live credential refresh cannot silently switch the server-verified identity", async () => {
  let token = "alice";
  const { model } = setup(peer(), { headers: () => ({ authorization: `Bearer ${token}` }) });
  await create({ title: "private" });
  const states: QueryState<unknown>[] = [];
  pending.subscribe(state => states.push(state), { keepPreviousDataOnError: true });
  await until(() => readyRows(states.at(-1)).length === 1);
  token = "bob";
  model.expireStreamCredentials();
  await until(() => states.at(-1)?.status === "error");
  expect(states.at(-1)?.data).toBeUndefined();
  expect(model.streams).toBe(0);
});

test("a missing receipt never resubmits a mutation and confirmation has one bounded deadline", async () => {
  const model = peer();
  let posts = 0, lookups = 0;
  let requestId: string | undefined;
  const fetcher = (async (url: string | URL | Request, init?: RequestInit) => {
    const path = new URL(String(url)).pathname;
    if (path === "/functions/mutate") { posts++; requestId = JSON.parse(String(init?.body)).request_id; throw new Error("delivery interrupted before acceptance"); }
    if (path.startsWith("/functions/receipts/")) { lookups++; return json(null); }
    return model.fetch(url, init);
  }) as typeof fetch;
  setup(model, { fetch: fetcher, confirmationTimeoutMs: 70 });
  const started = performance.now(), call = create({ title: "never accepted" });
  const states: string[] = [];
  call.subscribe(state => states.push(state.status));
  const error = await Promise.resolve(call).catch(error => error);
  expect(error).toBeInstanceOf(DatabaseError);
  expect(requestId).toBeDefined();
  expect(error).toMatchObject({ code: "confirmation_timeout", details: { requestId } });
  expect(states).toContain("recovering");
  expect(states.at(-1)).toBe("error");
  expect(posts).toBe(1); expect(lookups).toBeGreaterThan(2);
  expect(model.rows).toHaveLength(0);
  expect(performance.now() - started).toBeLessThan(500);
  const settledLookups = lookups;
  await new Promise(resolve => setTimeout(resolve, 40));
  expect(lookups).toBe(settledLookups);
});

test("delayed original delivery can commit while missing receipts are checked without replay", async () => {
  const model = peer(), gate = model.hold("delayed");
  setup(model, { operationTimeoutMs: 15, confirmationTimeoutMs: 500 });
  const call = create({ title: "delayed" });
  await until(() => call.status === "recovering" && model.lookups >= 2);
  expect(model.rows).toHaveLength(0);
  expect(model.submissions).toHaveLength(1);
  gate.release();
  expect((await call).title).toBe("delayed");
  expect(call.status).toBe("success");
  expect(model.submissions).toHaveLength(1);
});

test("receipt connectivity keeps recovering beyond three failures within the original window", async () => {
  const { model } = setup(peer(), { confirmationTimeoutMs: 800 });
  model.loseAcknowledgement(); model.recoveryOffline(true);
  const call = create({ title: "confirmed later" });
  await until(() => model.lookups >= 4);
  expect(call.status).toBe("recovering");
  model.recoveryOffline(false);
  expect((await call).title).toBe("confirmed later");
  expect(model.submissions).toHaveLength(1);
});

test("interrupted delivery removes prediction before confirmation settles", async () => {
  const model = peer(), gate = model.hold("still running");
  setup(model, { operationTimeoutMs: 15, confirmationTimeoutMs: 500 });
  const states: QueryState<unknown>[] = [];
  pending.subscribe(state => states.push(state));
  await until(() => states.at(-1)?.status === "success");
  const call = optimisticCreate({ title: "still running" });
  await until(() => readyRows(states.at(-1)).some(row => row.id === -1));
  await until(() => call.status === "recovering");
  expect(readyRows(states.at(-1))).toEqual([]);
  expect(model.rows).toEqual([]);
  gate.release();
  expect((await call).title).toBe("still running");
  expect(readyRows(states.at(-1)).some(row => row.id === -1)).toBe(false);
  expect(model.submissions).toHaveLength(1);
});

test("lost confirmation rolls back optimism immediately and a restarted runtime only fetches authority", async () => {
  const { adapter, model } = setup(peer(), { confirmationTimeoutMs: 80 });
  const states: QueryState<unknown>[] = [];
  pending.subscribe(state => states.push(state));
  await until(() => states.at(-1)?.status === "success");
  const gate = model.hold("in flight");
  const call = optimisticCreate({ title: "in flight" });
  const outcome = Promise.resolve(call).catch(error => error);
  await until(() => readyRows(states.at(-1)).some(row => row.id === -1));
  model.recoveryOffline(true);
  expect(await outcome).toMatchObject({ code: "confirmation_timeout" });
  expect(readyRows(states.at(-1)).some(row => row.id === -1)).toBe(false);
  gate.release();
  await until(() => model.rows.length === 1);
  await adapter.dispose();
  model.recoveryOffline(false);
  const restarted = new HttpRuntime({ baseUrl, auth: { key: "alice" }, headers: { authorization: "Bearer alice" }, contracts, fetch: model.fetch });
  cleanups.push(() => restarted.dispose());
  expect(await restarted.call({ name: "notes.pending", kind: "query", input: undefined })).toEqual(model.rows);
  expect(model.submissions).toHaveLength(1);
});

test("a fresh offline runtime never hydrates data from an earlier runtime", async () => {
  const { adapter, model } = setup();
  await create({ title: "private" }); await pending(); await adapter.dispose(); model.online(false);
  let token = "alice";
  const restarted = new HttpRuntime({ baseUrl, auth: { key: "alice" }, headers: () => ({ authorization: `Bearer ${token}` }), contracts, fetch: model.fetch, reconnectDelayMs: 5 });
  cleanups.push(() => restarted.dispose());
  const states: QueryState<unknown>[] = [];
  const stop = restarted.watch({ name: "notes.pending", kind: "query", input: undefined }, state => states.push(state));
  cleanups.push(stop);
  await until(() => states.at(-1)?.status === "error");
  expect(states.some(state => state.status === "success")).toBe(false);
  token = "bob"; restarted.setAuth({ key: "bob" }); model.online(true);
  expect(await restarted.call({ name: "notes.pending", kind: "query", input: undefined })).toEqual([]);
});

test("retiring identity during acknowledgement never delivers the old result", async () => {
  const model = peer(), release = model.holdAcknowledgement("Alice in flight");
  const { authenticate } = setup(model);
  const call = create({ title: "Alice in flight" });
  const outcome = Promise.resolve(call).catch(error => error);
  await until(() => model.rows.length === 1);
  authenticate(bob); release();
  expect(await outcome).toMatchObject({ code: "context_changed" });
  expect(call.data).toBeUndefined();
  expect(await pending()).toEqual([]);
  expect(model.submissions).toHaveLength(1);
});

test("a stalled response body consumes the same confirmation deadline", async () => {
  const model = peer();
  let posts = 0, cancelled = false;
  const fetcher = (async (url: string | URL | Request, init?: RequestInit) => {
    if (new URL(String(url)).pathname === "/functions/mutate") {
      posts++;
      return new Response(new ReadableStream({ cancel() { cancelled = true; } }), { headers: { "content-type": "application/json" } });
    }
    return model.fetch(url, init);
  }) as typeof fetch;
  setup(model, { fetch: fetcher, confirmationTimeoutMs: 40 });
  await expect(Promise.resolve(create({ title: "stalled" }))).rejects.toMatchObject({ code: "confirmation_timeout" });
  expect(posts).toBe(1); expect(cancelled).toBe(true);
});

test("receipt expiry is an error without replaying an already committed mutation", async () => {
  const { model } = setup(); model.loseAcknowledgement(); model.expireReceipts();
  await expect(Promise.resolve(create({ title: "expired result" }))).rejects.toMatchObject({ code: "receipt_expired", details: { committed: true } });
  expect(model.submissions).toHaveLength(1); expect(model.rows).toHaveLength(1);
});

test("invalid optimistic output does not prevent the server mutation", async () => {
  setup(); await pending();
  const brokenPrediction = create.withOptimistic({ query: pending, input: () => undefined, update: () => { throw new Error("prediction bug"); } });
  expect((await brokenPrediction({ title: "server owns outcome" })).title).toBe("server owns outcome");
});

const optimisticCreate = create.withOptimistic({
  query: pending,
  update: (rows, input) => [...rows, { id: -1, title: input.title, owner: "alice" }],
});

function readyRows(state: QueryState<unknown> | undefined): { id: number; title: string; owner: string }[] {
  return state?.status === "success" ? state.data as { id: number; title: string; owner: string }[] : [];
}


test("shared query watches reconnect from authoritative snapshots and dispose independently", async () => {
  const { model } = setup(), a: QueryState<unknown>[] = [], b: QueryState<unknown>[] = [];
  const stopA = pending.subscribe(state => a.push(state)), stopB = pending.subscribe(state => b.push(state));
  await until(() => a.at(-1)?.status === "success" && b.at(-1)?.status === "success"); expect(model.opens).toBe(1);
  await create({ title: "first" }); await until(() => (a.at(-1) as { data?: unknown[] }).data?.length === 1);
  stopA(); const count = a.length; model.disconnect();
  await until(() => model.opens === 2 && b.at(-1)?.status === "success" && !b.at(-1)!.isStale);
  await create({ title: "second" }); await until(() => (b.at(-1) as { data?: unknown[] }).data?.length === 2);
  expect(a).toHaveLength(count); stopB(); await until(() => model.streams === 0);
});

test("portable result codecs preserve bigint and reject unknown fields, non-finite numbers and invalid types", () => {
  const codec = valueCodec({ kind: "object", fields: { count: { shape: { kind: "bigint" }, optional: false } } });
  expect(codec.decode({ count: "9223372036854775807" })).toEqual({ count: 9223372036854775807n });
  expect(codec.encode({ count: -9223372036854775808n })).toEqual({ count: "-9223372036854775808" });
  expect(codec.decode({ count: "18446744073709551614" })).toEqual({ count: 18446744073709551614n });
  expect(codec.encode({ count: 18446744073709551614n })).toEqual({ count: "18446744073709551614" });
  for (const value of [{ count: "01" }, { count: "1", leaked: true }, { count: 1 }]) expect(() => codec.decode(value)).toThrow();
  expect(valueCodec({ kind: "number" }).decode(1e20)).toBe(1e20);
  expect(() => valueCodec({ kind: "number" }).decode(Infinity)).toThrow();
  const union = valueCodec({ kind: "union", choices: [{ kind: "bigint" }, { kind: "string" }] });
  expect(() => union.decode("123")).toThrow("Ambiguous union");
  expect(() => union.encode(123n)).toThrow("Ambiguous union");
  expect(union.decode("text")).toBe("text");
  expect(() => valueCodec({ kind: "array", item: { kind: "string" } }).decode(new Array(1))).toThrow();
});

test("SSE handles fragmented UTF-8 and CR/LF while rejecting truncated and oversized frames", async () => {
  const content = new TextEncoder().encode('event: snapshot\r\nid: id\r\ndata: 🦀\r\ndata: second\r\n\r\n');
  const bytes = new ReadableStream<Uint8Array>({ start(controller) { for (const byte of content) controller.enqueue(Uint8Array.of(byte)); controller.close(); } });
  const events = []; for await (const event of sseEvents(bytes, 200)) events.push(event);
  expect(events).toEqual([{ event: "snapshot", id: "id", data: "🦀\nsecond" }]);
  for (const [content, limit] of [["data: truncated", 100], ["data: exceeds\n\n", 3]] as const) {
    const bytes = new ReadableStream<Uint8Array>({ start(controller) { controller.enqueue(new TextEncoder().encode(content)); controller.close(); } });
    const parse = async () => { for await (const _ of sseEvents(bytes, limit)) {} }; await expect(parse()).rejects.toThrow();
  }
});

test("an observer changing identity cannot deliver the retired result to another observer", async () => {
  const { authenticate } = setup();
  let changed = false;
  const observed: QueryState<unknown>[] = [];
  pending.subscribe(state => { if (state.status === "success" && !changed) { changed = true; authenticate(bob); } });
  pending.subscribe(state => observed.push(state));
  await until(() => observed.at(-1)?.status === "error");
  expect(observed.some(state => state.status === "success")).toBe(false);
});

test("optimistic direct calls render before submission completes and rejection preserves remote confirmed changes", async () => {
  const model = peer(), gate = model.hold("Rejected");
  setup(model);
  const states: QueryState<unknown>[] = [];
  pending.subscribe(state => states.push(state)); await until(() => states.at(-1)?.status === "success");
  const rejected = optimisticCreate({ title: "Rejected" });
  const outcome = Promise.resolve(rejected).catch(error => error);
  await until(() => readyRows(states.at(-1)).some(row => row.title === "Rejected"));
  expect(model.rows).toEqual([]);
  await create({ title: "Remote" });
  await until(() => readyRows(states.at(-1)).length === 2);
  gate.reject(); expect(await outcome).toMatchObject({ code: "conflict" });
  await until(() => readyRows(states.at(-1)).length === 1);
  expect(readyRows(states.at(-1))[0]!.title).toBe("Remote");
});

test("query cancellation settles during a shared pending session handshake", async () => {
  const model = peer();
  let handshakes = 0, queries = 0, release!: () => void;
  const handshake = new Promise<void>(resolve => { release = resolve; });
  const fetcher = (async (url: string | URL | Request, init?: RequestInit) => {
    const path = new URL(String(url)).pathname;
    if (path === "/functions/session") { handshakes++; await handshake; }
    if (path === "/functions/query") queries++;
    return model.fetch(url, init);
  }) as typeof fetch;
  setup(model, { fetch: fetcher });
  const controller = new AbortController(), reason = new Error("query caller cancelled");
  const cancelled = Promise.resolve(pending({ signal: controller.signal })).catch(error => error);
  try {
    await until(() => handshakes === 1);
    controller.abort(reason);
    const outcome = await Promise.race([cancelled, new Promise(resolve => setTimeout(() => resolve("abort did not settle"), 200))]);
    expect(outcome).toBe(reason);
    // Cancelling one caller must leave the shared handshake usable by another.
    const survivor = pending();
    release();
    expect(await survivor).toEqual([]);
    expect(handshakes).toBe(1);
    expect(queries).toBe(1);
  } finally { release(); }
});

test("an initial watch observer can synchronously abort without opening a stream", async () => {
  const { model } = setup();
  const controller = new AbortController();
  const states: QueryState<unknown>[] = [];
  const stop = pending.subscribe(state => {
    states.push(state);
    controller.abort();
  }, { signal: controller.signal });
  try {
    await new Promise(resolve => setTimeout(resolve, 20));
    expect(states).toHaveLength(1);
    expect(model.opens).toBe(0);
    expect(model.streams).toBe(0);
  } finally { stop(); }
});

test("an invalid result after a verified receipt preserves commit evidence without recovery", async () => {
  const model = peer();
  let posts = 0, lookups = 0;
  const fetcher = (async (url: string | URL | Request, init?: RequestInit) => {
    const path = new URL(String(url)).pathname;
    if (path.startsWith("/functions/receipts/")) lookups++;
    const response = await model.fetch(url, init);
    if (path !== "/functions/mutate") return response;
    posts++;
    const receipt = await response.json() as Record<string, unknown>;
    return json({ ...receipt, result: { id: "invalid", title: "committed", owner: "alice" } });
  }) as typeof fetch;
  setup(model, { fetch: fetcher });
  const call = create({ title: "committed" });
  await expect(Promise.resolve(call)).rejects.toMatchObject({ code: "invalid_result", details: { committed: true } });
  expect(call.status).toBe("error");
  expect(posts).toBe(1);
  expect(lookups).toBe(0);
  expect(model.rows).toHaveLength(1);
});

test("unsettled response cancellation cannot block the confirmation deadline", async () => {
  for (const contentType of ["application/json", "text/plain"]) {
    const model = peer();
    let posts = 0, cancelled = false, releaseCancellation!: () => void;
    const cancellation = new Promise<void>(resolve => { releaseCancellation = resolve; });
    const fetcher = (async (url: string | URL | Request, init?: RequestInit) => {
      if (new URL(String(url)).pathname === "/functions/mutate") {
        posts++;
        return new Response(new ReadableStream({ cancel() { cancelled = true; return cancellation; } }), { headers: { "content-type": contentType } });
      }
      return model.fetch(url, init);
    }) as typeof fetch;
    const adapter = new HttpRuntime({ baseUrl, contracts, fetch: fetcher, confirmationTimeoutMs: 30, reconnectDelayMs: 2 });
    cleanups.push(() => adapter.dispose());
    const outcome = adapter.call({ name: "notes.create", kind: "mutation", input: { title: "stalled cancellation" } }).catch(error => error);
    try {
      const error = await Promise.race([outcome, new Promise(resolve => setTimeout(() => resolve({ code: "test_timeout" }), 200))]);
      expect(error).toMatchObject({ code: "confirmation_timeout" });
      expect(posts).toBe(1);
      expect(cancelled).toBe(true);
    } finally {
      // Release only for failed-test cleanup; deadline settlement must precede it.
      releaseCancellation();
  }
  }
});

test("tickets renew for future calls while recovery retains its original ticket and ACK", async () => {
  const model = peer();
  const tickets: string[] = [], submissions: string[] = [], lookups: string[] = [], acks: string[] = [];
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  let recover = false;
  const fetcher = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = new URL(String(input));
    if (url.pathname === "/functions/session") {
      const response = await model.fetch(input, init);
      const head = await response.json();
      const id = crypto.randomUUID(); tickets.push(id);
      return json({ ...head, ticket: { id, expires_in_ms: 100, renew_after_ms: 30 } });
    }
    if (url.pathname === "/functions/mutate") {
      submissions.push(JSON.parse(String(init?.body)).ticket);
      if (!recover) { recover = true; await model.fetch(input, init); await gate; throw new Error("lost response"); }
    }
    if (url.pathname.endsWith("/ack")) acks.push(JSON.parse(String(init?.body)).ticket);
    else if (url.pathname.startsWith("/functions/receipts/")) lookups.push(url.searchParams.get("ticket")!);
    return model.fetch(input, init);
  }) as typeof fetch;
  setup(model, { fetch: fetcher, confirmationTimeoutMs: 1000 });
  const first = Promise.resolve(create({ title: "first" }));
  await until(() => tickets.length >= 2);
  release();
  await first;
  await create({ title: "second" });
  expect(submissions).toEqual([tickets[0]!, tickets[1]!]);
  expect(lookups).toEqual([tickets[0]!]);
  await until(() => acks.length === 2);
  expect(acks).toEqual(submissions);
});

test("lost mutation failures recover durably without resubmission or ACK", async () => {
  const model = peer();
  let submissions = 0, lookups = 0, acks = 0;
  const fetcher = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = new URL(String(input));
    if (url.pathname === "/functions/mutate") { submissions++; throw new Error("failure response lost"); }
    if (url.pathname.endsWith("/ack")) { acks++; return json(null); }
    if (url.pathname.startsWith("/functions/receipts/")) {
      lookups++;
      return lookups === 1 ? json(null) : json({ code: "execution_failed" }, 422);
    }
    return model.fetch(input, init);
  }) as typeof fetch;
  setup(model, { fetch: fetcher });
  await expect(Promise.resolve(create({ title: "failed" }))).rejects.toMatchObject({ code: "execution_failed" });
  expect(submissions).toBe(1); expect(lookups).toBe(2); expect(acks).toBe(0);
});

test("an expired ticket obtains a fresh handshake before a new invocation", async () => {
  const model = peer();
  const tickets: string[] = [], used: string[] = [];
  let unavailable = false;
  const fetcher = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = new URL(String(input));
    if (url.pathname === "/functions/session") {
      if (unavailable) throw new Error("renewal offline");
      const response = await model.fetch(input, init), head = await response.json();
      const id = crypto.randomUUID(); tickets.push(id);
      return json({ ...head, ticket: { id, expires_in_ms: 50, renew_after_ms: 15 } });
    }
    if (url.pathname === "/functions/mutate") used.push(JSON.parse(String(init?.body)).ticket);
    return model.fetch(input, init);
  }) as typeof fetch;
  setup(model, { fetch: fetcher });
  await create({ title: "before expiry" });
  unavailable = true;
  await new Promise(resolve => setTimeout(resolve, 70));
  unavailable = false;
  await create({ title: "after expiry" });
  expect(tickets).toHaveLength(2); expect(used).toEqual(tickets);
});

test("a late renewal cannot install a retired identity ticket", async () => {
  const model = peer();
  let sessions = 0, release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  const submitted: string[] = [];
  const fetcher = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = new URL(String(input));
    if (url.pathname === "/functions/session") {
      const ordinal = ++sessions;
      const head = await (await model.fetch(input, init)).json();
      if (ordinal === 2) await gate;
      return json({ ...head, ticket: { id: `00000000-0000-0000-0000-${String(ordinal).padStart(12, "0")}`, expires_in_ms: 1000, renew_after_ms: 20 } });
    }
    if (url.pathname === "/functions/mutate") submitted.push(JSON.parse(String(init?.body)).ticket);
    return model.fetch(input, init);
  }) as typeof fetch;
  const { authenticate } = setup(model, { fetch: fetcher });
  await create({ title: "alice" });
  await until(() => sessions === 2);
  authenticate(bob);
  await create({ title: "bob" });
  release();
  await new Promise(resolve => setTimeout(resolve, 1));
  await create({ title: "bob again" });
  expect(submitted).toEqual([1, 3, 3].map(n => `00000000-0000-0000-0000-${String(n).padStart(12, "0")}`));
});

test("ACK failure cannot undo delivery and expired admission does not claim a commit", async () => {
  const model = peer();
  let expired = false, stalledAcks = false, acks = 0, submissions = 0;
  const fetcher = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = new URL(String(input));
    if (url.pathname.endsWith("/ack")) { acks++; if (stalledAcks) return new Promise<Response>(() => {}); throw new Error("ACK lost"); }
    if (url.pathname === "/functions/mutate") {
      submissions++;
      if (expired) return json({ code: "ticket_expired" }, 410);
    }
    return model.fetch(input, init);
  }) as typeof fetch;
  setup(model, { fetch: fetcher });
  expect((await create({ title: "delivered" })).title).toBe("delivered");
  await until(() => acks === 1);
  expired = true;
  try { await create({ title: "expired" }); throw new Error("expected rejection"); }
  catch (error) {
    expect(error).toBeInstanceOf(DatabaseError);
    expect((error as DatabaseError).code).toBe("ticket_expired");
    expect((error as DatabaseError).details).toBeUndefined();
  }
  expect(submissions).toBe(2); expect(acks).toBe(1); expect(model.lookups).toBe(0);
  expired = false; stalledAcks = true;
  for (let i = 0; i < 128; i++) await create({ title: String(i) });
  await expect(Promise.resolve(create({ title: "overloaded" }))).rejects.toMatchObject({ code: "capacity" });
  expect(submissions).toBe(130);

});


test("a complete round stages sibling handles before callable and handle observers run", async () => {
  const model = peer(), codecs = contracts["notes.pending"];
  const adapter = new HttpRuntime({ baseUrl, contracts: { first: codecs, second: codecs }, fetch: model.fetch, reconnectDelayMs: 2 });
  const scope = createRuntimeScope(); const unconfigure = scope.configure(() => adapter);
  cleanups.push(() => adapter.dispose(), unconfigure);
  type Definition = { kind: "query"; $types: { input: undefined; output: unknown[] } };
  const first = queryBinding<Definition>(scope, "first", false, codecs);
  const second = queryBinding<Definition>(scope, "second", false, codecs);
  const a = first(), b = second();
  const seen: unknown[] = [];
  a.subscribe(state => { if (state.status === "success") seen.push([state.data.length, b.data?.length]); });
  b.subscribe(() => {});
  first.subscribe(state => { if (state.status === "success") seen.push([a.data?.length, b.data?.length]); });
  await until(() => seen.length === 2);
  expect(model.opens).toBe(1);
  expect(seen).toEqual([[0, 0], [0, 0]]);
  model.rows.push({ id: 1, title: "shared", owner: "anonymous" });
  model.disconnect();
  await until(() => a.data?.length === 1);
  expect(seen.every(pair => (pair as number[])[0] === (pair as number[])[1])).toBe(true);
});


test("partial rounds cannot advance a member and member access errors clear retained data then recover", async () => {
  const model = peer(), codecs = contracts["notes.pending"];
  const adapter = new HttpRuntime({ baseUrl, contracts: { first: codecs, second: codecs }, fetch: model.fetch, reconnectDelayMs: 50 });
  cleanups.push(() => adapter.dispose());
  const first: QueryState<unknown>[] = [], second: QueryState<unknown>[] = [];
  const operation = (name: string) => ({ name, kind: "query" as const, input: undefined });
  adapter.watch(operation("first"), state => first.push(state));
  adapter.watch(operation("second"), state => second.push(state));
  await until(() => first.at(-1)?.status === "success" && second.at(-1)?.status === "success");
  model.roundResults(results => [{ ...results[0]!, result: [{ id: 1, title: "partial", owner: "anonymous" }] }]);
  await until(() => first.at(-1)?.isStale === true);
  expect(first.at(-1)?.data).toEqual([]); expect(second.at(-1)?.data).toEqual([]);
  await until(() => model.opens === 2 && first.at(-1)?.isStale === false);
  model.roundResults(results => [{ key: results[0]!.key, error: { code: "forbidden" } }, results[1]!]);
  await until(() => first.at(-1)?.status === "error");
  expect(first.at(-1)?.data).toBeUndefined(); expect(second.at(-1)?.status).toBe("success");
  model.roundResults(results => results);
  await until(() => first.at(-1)?.status === "success");
  expect(model.opens).toBe(2);
  model.revision(5);
  model.roundResults(results => results.map(result => ({ key: result.key, error: { code: "forbidden" } })));
  await until(() => first.at(-1)?.status === "error" && second.at(-1)?.status === "error");
  model.revision(4);
  model.roundResults(results => results);
  await until(() => model.opens > 2);
  expect(first.at(-1)?.status).toBe("error"); expect(second.at(-1)?.data).toBeUndefined();
  model.revision(6);
  await until(() => first.at(-1)?.status === "success");
});


test("observed handles wait for a shared first round while their one-shot outcomes resolve", async () => {
  const model = peer(), codecs = contracts["notes.pending"];
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  const fetcher = (async (url: string | URL | Request, init?: RequestInit) => {
    if (new URL(String(url)).pathname === "/functions/subscriptions") await gate;
    return model.fetch(url, init);
  }) as typeof fetch;
  const adapter = new HttpRuntime({ baseUrl, contracts: { first: codecs, second: codecs }, fetch: fetcher });
  const scope = createRuntimeScope(); const unconfigure = scope.configure(() => adapter);
  cleanups.push(() => adapter.dispose(), unconfigure);
  type Definition = { kind: "query"; $types: { input: undefined; output: unknown[] } };
  const first = queryBinding<Definition>(scope, "first", false, codecs);
  const second = queryBinding<Definition>(scope, "second", false, codecs);
  const a = first(), b = second();
  const seen: unknown[] = [];
  a.subscribe(state => { if (state.status === "success") seen.push([a.data, b.data]); });
  b.subscribe(() => {});
  try {
    expect(await a).toEqual([]); expect(await b).toEqual([]);
    expect(a.status).toBe("pending"); expect(b.status).toBe("pending"); expect(seen).toEqual([]);
    release();
    await until(() => seen.length === 1);
    expect(seen).toEqual([[[], []]]);
    const retired: unknown[] = [];
    a.subscribe(state => { if (state.status === "error") retired.push([a.data, b.data]); });
    adapter.setAuth({ key: "changed" });
    expect(retired).toEqual([[undefined, undefined]]);
  } finally { release(); }
});


test("inactive query churn is bounded and expired bases cannot receive optimism while live queries stay pinned", async () => {
  const model = peer();
  const adapter = new HttpRuntime({ baseUrl, fetch: model.fetch, contracts: {
    ...contracts, search: { ...contracts["notes.pending"], input: valueCodec({ kind: "string" }) },
  } });
  cleanups.push(() => adapter.dispose());
  const query = (input: string) => ({ name: "search", kind: "query" as const, input });
  let live: QueryState<unknown> | undefined;
  const stop = adapter.watch(query("live"), state => { live = state; });
  await until(() => live?.status === "success");
  for (let i = 0; i < 130; i++) await adapter.call(query(String(i)));
  let bases: readonly { name: string; input?: unknown }[] = [];
  const inspect = async () => {
    await adapter.call({ name: "notes.create", kind: "mutation", input: { title: "inspect" } }, {
      optimistic: (_, context) => { bases = context.queries("search"); },
    });
  };
  await inspect();
  expect(bases).toHaveLength(129);
  expect(bases.some(base => base.input === "0")).toBe(false);
  expect(bases.some(base => base.input === "129")).toBe(true);
  expect(bases.some(base => base.input === "live")).toBe(true);
  for (let i = 0; i < 18; i++) await adapter.call(query(`${i}:` + "x".repeat(1024 * 1024)));
  await inspect();
  expect(bases.length).toBeLessThan(18);
  expect(bases.some(base => base.input === "live")).toBe(true);
  const now = Date.now(), clock = spyOn(Date, "now").mockReturnValue(now + 301_000);
  try {
    await inspect();
    expect(bases.map(base => base.input)).toEqual(["live"]);
  } finally { clock.mockRestore(); }
  stop();
  const states: QueryState<unknown>[] = [];
  adapter.watch(query("live"), state => states.push(state));
  expect(states[0]).toEqual({ status: "pending" });
  await until(() => states.at(-1)?.status === "success");
});

test("stalled stream cancellation cannot prevent idle reconnect or disposal", async () => {
  const model = peer();
  let opens = 0, cancels = 0;
  const adapter = new HttpRuntime({ baseUrl, contracts, streamIdleTimeoutMs: 10, reconnectDelayMs: 1,
    fetch: (async (input, init) => {
      if (new URL(String(input)).pathname !== "/functions/subscriptions") return model.fetch(input, init);
      opens++;
      return new Response(new ReadableStream({ cancel() { cancels++; return new Promise<void>(() => {}); } }), { headers: { "content-type": "text/event-stream" } });
    }) as typeof fetch,
  });
  cleanups.push(() => adapter.dispose());
  adapter.watch({ name: "notes.pending", kind: "query", input: undefined }, () => {});
  await until(() => opens >= 2);
  await adapter.dispose();
  await until(() => cancels === opens);
});

test("concurrent call admission is bounded and disposal settles callers behind a stuck handshake", async () => {
  const adapter = new HttpRuntime({ baseUrl, contracts, fetch: ((_input: string | URL | Request) => new Promise<Response>(() => {})) as typeof fetch });
  cleanups.push(() => adapter.dispose());
  const calls = Array.from({ length: 128 }, () => adapter.call({ name: "notes.pending", kind: "query", input: undefined }).catch(error => error));
  await expect(adapter.call({ name: "notes.pending", kind: "query", input: undefined })).rejects.toMatchObject({ code: "capacity" });
  await adapter.dispose();
  expect((await Promise.all(calls)).every(error => error instanceof Error)).toBe(true);
});

test("pagination pins discovered ranges across rebuilds, publishes coherent rounds and retries failed appends", async () => {
  const { PaginationOperation } = await import("../src/pagination.ts");
  const pageCodec = valueCodec({ kind: "object", fields: { page: { shape: { kind: "array", item: { kind: "string" } }, optional: false }, cursor: { shape: { kind: "union", choices: [{ kind: "null" }, { kind: "string" }] }, optional: false } } });
  const inputCodec = valueCodec({ kind: "object", fields: { cursor: { shape: { kind: "union", choices: [{ kind: "null" }, { kind: "string" }] }, optional: false } } });
  type Query = { key: string; input: { cursor: string | null }; page?: { range: string } };
  const requests: Query[][] = [];
  const streams = new Set<{ controller: ReadableStreamDefaultController<Uint8Array>; queries: Query[]; id: string; sequence: number }>();
  let failAppend = false, overflow = false;
  const head = { source, scope, principal: alice.principal, authorization_scope: alice.authorizationScope, revision: "0" };
  const emit = () => {
    for (const stream of streams) {
      const results = stream.queries.map(query => query.input.cursor !== null && failAppend || overflow
        ? { key: query.key, error: { code: "execution_limit" } }
        : { key: query.key, result: { page: query.input.cursor === null ? ["a", "b"] : ["c"], cursor: query.input.cursor === null ? "after-b" : null }, page: { range: query.page?.range ?? (query.input.cursor === null ? "first-range" : "last-range") } });
      const frame = { subscription: stream.id, sequence: String(++stream.sequence), round: { ...head, results } };
      stream.controller.enqueue(new TextEncoder().encode(`event: round\nid: ${stream.id}:${stream.sequence}\ndata: ${JSON.stringify(frame)}\n\n`));
    }
  };
  const fetcher = (async (url: string | URL | Request, init?: RequestInit) => {
    if (String(url).endsWith("/functions/session")) return json({ ...head, ticket: { id: crypto.randomUUID(), expires_in_ms: 60000, renew_after_ms: 30000 } });
    if (!String(url).endsWith("/functions/subscriptions")) throw new Error("Pagination must load through complete live rounds");
    const { queries } = JSON.parse(String(init?.body)) as { queries: Query[] }; requests.push(queries);
    return new Response(new ReadableStream<Uint8Array>({ start(controller) {
      const stream = { controller, queries, id: crypto.randomUUID(), sequence: 0 }; streams.add(stream);
      init?.signal?.addEventListener("abort", () => { streams.delete(stream); controller.close(); }, { once: true });
      emit();
    } }), { headers: { "content-type": "text/event-stream" } });
  }) as typeof fetch;
  const runtime = new HttpRuntime({ baseUrl, contracts: { paged: { kind: "query", input: inputCodec, result: pageCodec } }, fetch: fetcher });
  cleanups.push(() => runtime.dispose());
  const list = new PaginationOperation<{ cursor: string | null }, string>(runtime, "paged", cursor => ({ cursor }), { input: inputCodec, result: pageCodec });
  cleanups.push(() => list.dispose());
  const seen: QueryState<readonly string[]>[] = [];
  const unsubscribe = list.subscribe(state => { seen.push(state); });
  await list;
  await until(() => requests.some(queries => queries[0]?.page?.range === "first-range"));
  expect(list.data).toEqual(["a", "b"]);
  failAppend = true;
  const first = list.next(), overlap = list.next(2);
  expect(overlap).toBe(first);
  await expect(first).rejects.toMatchObject({ code: "execution_limit" });
  expect(list.data).toEqual(["a", "b"]);
  expect(list.hasMore).toBe(true);
  failAppend = false;
  await list.next();
  expect(list.data).toEqual(["a", "b", "c"]);
  expect(list.hasMore).toBe(false);
  expect(requests.filter(queries => queries.length === 2).every(queries => queries[0]?.page?.range === "first-range")).toBe(true);
  overflow = true; emit();
  await until(() => list.status === "error");
  expect(list.data).toBeUndefined();
  overflow = false; emit();
  await until(() => list.status === "success");
  expect(list.data).toEqual(["a", "b", "c"]);
  expect(seen.filter(state => state.status === "success").every(state => state.data.length === 2 || state.data.length === 3)).toBe(true);
  unsubscribe();
  expect(streams.size).toBe(0);
});

test("one-shot queries recover interrupted transport inside one overall deadline", async () => {
  const model = peer();
  let attempts = 0;
  const fetcher = (async (url, init) => {
    if (new URL(String(url)).pathname === "/functions/query" && ++attempts === 1) throw new Error("connection reset");
    return model.fetch(url, init);
  }) as typeof fetch;
  setup(model, { fetch: fetcher, queryTimeoutMs: 200, reconnectDelayMs: 1 });
  expect(await pending()).toEqual([]);
  expect(attempts).toBe(2);
});

test("repeated query transport failures terminate at the shared deadline and stop retrying", async () => {
  const model = peer();
  let attempts = 0;
  const fetcher = (async (url, init) => {
    if (new URL(String(url)).pathname === "/functions/query") { attempts++; throw new Error("offline"); }
    return model.fetch(url, init);
  }) as typeof fetch;
  setup(model, { fetch: fetcher, queryTimeoutMs: 30, reconnectDelayMs: 1 });
  await expect(Promise.resolve(pending())).rejects.toMatchObject({ code: "query_timeout" });
  expect(attempts).toBeGreaterThan(1);
  const stoppedAt = attempts;
  await new Promise(resolve => setTimeout(resolve, 20));
  expect(attempts).toBe(stoppedAt);
});

test("aborting a one-shot query leaves its shared live watch and other callers active", async () => {
  const model = peer();
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  let attempts = 0;
  const fetcher = (async (url, init) => {
    if (new URL(String(url)).pathname === "/functions/query" && ++attempts === 1) await gate;
    return model.fetch(url, init);
  }) as typeof fetch;
  setup(model, { fetch: fetcher, queryTimeoutMs: 200 });
  const states: QueryState<unknown>[] = [];
  const stop = pending.subscribe(state => states.push(state));
  await until(() => states.at(-1)?.status === "success");
  const controller = new AbortController(), reason = new Error("caller cancelled");
  const cancelled = Promise.resolve(pending({ signal: controller.signal })).catch(error => error);
  try {
    await until(() => attempts === 1);
    controller.abort(reason);
    expect(await cancelled).toBe(reason);
    expect(model.streams).toBe(1);
    expect(await pending()).toEqual([]);
    await create({ title: "live survives" });
    await until(() => readyRows(states.at(-1)).length === 1);
    expect(model.streams).toBe(1);
  } finally { release(); stop(); }
});

test("server execution failures and malformed results are terminal for one-shot queries", async () => {
  const model = peer();
  let attempts = 0, malformed = false;
  const fetcher = (async (url, init) => {
    if (new URL(String(url)).pathname === "/functions/query") {
      attempts++;
      return malformed ? new Response("not JSON", { headers: { "content-type": "application/json" } }) : json({ code: "execution_limit" }, 422);
    }
    return model.fetch(url, init);
  }) as typeof fetch;
  setup(model, { fetch: fetcher, queryTimeoutMs: 200, reconnectDelayMs: 1 });
  await expect(Promise.resolve(pending())).rejects.toMatchObject({ code: "execution_limit" });
  expect(attempts).toBe(1);
  malformed = true;
  await expect(Promise.resolve(pending())).rejects.toBeInstanceOf(SyntaxError);
  expect(attempts).toBe(2);
});
