import { expect, spyOn, test } from "bun:test";
import { createQueryObserver } from "../src/bindings.ts";
import { HttpRuntime } from "../src/http.ts";
import { createRuntimeScope } from "../src/runtime.ts";
import { queryBinding } from "../src/callable.ts";
import { DatabaseError, type QueryState } from "../src/index.ts";
import { valueCodec } from "../src/codec.ts";

test("SSR fallback is fenced by real auth changes, promise loading and expiry without replaying old stream callbacks", async () => {
  const scope = createRuntimeScope();
  const codec = valueCodec({ kind: "string" });
  const runtime = new HttpRuntime({ baseUrl: "http://localhost:3001", auth: { key: "alice" },
    contracts: { title: { kind: "query", input: valueCodec({ kind: "undefined" }), result: codec } } });
  const streams: { emit: (state: QueryState<unknown>) => void; active: boolean }[] = [];
  // Control only incoming live delivery. Authentication/expiry remains the real runtime.
  const watch = spyOn(runtime, "watch").mockImplementation((_operation, emit) => {
    const stream = { emit, active: true }; streams.push(stream);
    emit({ status: "pending" });
    return () => { stream.active = false; };
  });
  const detach = scope.configure(() => runtime);
  const query = queryBinding<{ kind: "query"; $types: { input: undefined; output: string } }>(scope, "title", false);
  const observer = createQueryObserver(query, { initialData: "Alice SSR", initialAuthKey: "alice", keepPreviousDataOnError: true });
  let stop = () => {};
  try {
    expect(observer.getSnapshot().data).toBe("Alice SSR");
    stop = observer.subscribe(() => {});
    const aliceStream = streams[0]!;
    let resolve!: (auth: { key: string }) => void;
    const updating = scope.setAuth(new Promise<{ key: string }>(ready => { resolve = ready; }));
    expect(observer.getSnapshot().data).toBeUndefined();
    expect(aliceStream.active).toBe(false);
    expect(streams).toHaveLength(1); // No anonymous request while identity resolves.
    resolve({ key: "bob" }); await updating;
    expect(streams).toHaveLength(2);
    aliceStream.emit({ status: "success", data: "Alice late", isStale: false, isOptimistic: false });
    expect(observer.getSnapshot().data).toBeUndefined();
    streams[1]!.emit({ status: "success", data: "Bob live", isStale: false, isOptimistic: false });
    expect(observer.getSnapshot().data).toBe("Bob live");
    streams[1]!.emit({ status: "error", error: new DatabaseError({ code: "forbidden", message: "revoked" }) });
    expect(observer.getSnapshot().data).toBeUndefined();
    scope.setAuth({ key: "alice" });
    expect(observer.getSnapshot().data).toBeUndefined(); // SSR seed cannot revive.
    scope.setAuth({ key: "alice", expiresAt: Date.now() + 10 });
    await new Promise(resolve => setTimeout(resolve, 25));
    expect(runtime.clientAuth.key).toBeNull();
    expect(observer.getSnapshot().data).toBeUndefined();
    expect(streams.filter(stream => stream.active)).toHaveLength(1);
  } finally {
    stop(); detach(); watch.mockRestore(); await runtime.dispose();
  }
});

test("a protected SSR seed is never assigned to an anonymous or different account", async () => {
  const scope = createRuntimeScope();
  const runtime = new HttpRuntime({ baseUrl: "http://localhost:3001", auth: { key: "bob" }, contracts: {} });
  const detach = scope.configure(() => runtime);
  const query = queryBinding<{ kind: "query"; $types: { input: undefined; output: string } }>(scope, "title", false);
  try {
    const protectedView = createQueryObserver(query, { initialData: "Alice SSR", initialAuthKey: "alice" });
    expect(protectedView.getServerSnapshot().data).toBeUndefined();
    expect(protectedView.getSnapshot().data).toBeUndefined();
    scope.setAuth({ key: "alice" });
    expect(protectedView.getSnapshot().data).toBeUndefined();
    const unscoped = createQueryObserver(query, { initialData: "not scoped" });
    expect(unscoped.getSnapshot().data).toBeUndefined();
  } finally { detach(); await runtime.dispose(); }
});

test("server rejection clears the SSR view and remains an error without an authorization retry loop", async () => {
  const scope = createRuntimeScope();
  let requests = 0;
  const runtime = new HttpRuntime({ baseUrl: "http://localhost:3001", auth: { key: "alice" },
    contracts: { title: { kind: "query", input: valueCodec({ kind: "undefined" }), result: valueCodec({ kind: "string" }) } },
    fetch: (async (_input: string | URL | Request) => {
      requests++;
      return new Response(JSON.stringify({ code: "unauthorized" }), { status: 401, headers: { "content-type": "application/json" } });
    }) as typeof fetch,
  });
  const detach = scope.configure(() => runtime);
  const query = queryBinding<{ kind: "query"; $types: { input: undefined; output: string } }>(scope, "title", false);
  const observer = createQueryObserver(query, { initialData: "old authorized render", initialAuthKey: "alice" });
  const stop = observer.subscribe(() => {});
  try {
    const deadline = Date.now() + 1000;
    while (observer.getSnapshot().status !== "error" && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 1));
    expect(observer.getSnapshot().status).toBe("error");
    expect(observer.getSnapshot().data).toBeUndefined();
    await new Promise(resolve => setTimeout(resolve, 10));
    expect(observer.getSnapshot().status).toBe("error");
    expect(requests).toBe(1);
  } finally { stop(); detach(); await runtime.dispose(); }
});

test("pagination observers mount the core list once, append pages, and release all live ranges", async () => {
  const { createPaginationObserver } = await import("../src/bindings.ts");
  type Definition = { kind: "query"; $types: { input: { filter: string; cursor: string | null }; output: import("../src/page.ts").Page<{ title: string }> } };
  const scope = createRuntimeScope();
  let resolutions = 0;
  let calls = 0;
  const streams: { input: unknown; emit: (state: QueryState<unknown>) => void; active: boolean }[] = [];
  const detach = scope.configure(() => {
    resolutions++;
    return runtime;
  });
  const runtime = {
    call: async () => { calls++; throw new Error("No one-shot read"); },
    watch(operation: import("../src/contracts.ts").Operation, emit: (state: QueryState<unknown>) => void) {
      const stream = { input: operation.input, emit, active: true }; streams.push(stream);
      emit({ status: "pending" });
      return () => { stream.active = false; };
    },
  };
  const query = queryBinding<Definition>(scope, "pages", true, undefined, true);
  const view = createPaginationObserver(query, cursor => ({ filter: "active", cursor }), { initialData: [{ title: "SSR" }] });
  expect(resolutions).toBe(0);
  expect(view.getServerSnapshot()).toMatchObject({ status: "pending", data: [{ title: "SSR" }], hasMore: false });
  expect(streams).toHaveLength(0);
  const stop = view.subscribe(() => {});
  try {
    await Promise.resolve();
    expect(streams).toHaveLength(1);
    expect(view.getSnapshot().data).toEqual([{ title: "SSR" }]);
    streams[0]!.emit({ status: "success", data: { page: [{ title: "first" }], cursor: "second" }, isStale: false, isOptimistic: false });
    await new Promise(resolve => setTimeout(resolve, 0));
    const next = view.getSnapshot().next;
    expect(view.getSnapshot()).toMatchObject({ status: "success", data: [{ title: "first" }], hasMore: true });
    const append = next();
    await Promise.resolve();
    expect(streams[1]!.input).toEqual({ filter: "active", cursor: "second" });
    streams[1]!.emit({ status: "success", data: { page: [{ title: "last" }], cursor: null }, isStale: false, isOptimistic: false });
    await append;
    expect(view.getSnapshot()).toMatchObject({ status: "success", data: [{ title: "first" }, { title: "last" }], hasMore: false });
    expect(view.getSnapshot().next).toBe(next);
    expect(calls).toBe(0);
  } finally { stop(); detach(); }
  expect(streams.every(stream => !stream.active)).toBe(true);
});

test("paginated binding snapshots stage sibling rounds before notifying UI subscribers", async () => {
  const { createPaginationObserver } = await import("../src/bindings.ts");
  const { batchNotifications, notifyObserver } = await import("../src/notifications.ts");
  type Definition = { kind: "query"; $types: { input: { cursor: string | null }; output: import("../src/page.ts").Page<string> } };
  const scope = createRuntimeScope();
  const streams = new Map<string, (state: QueryState<unknown>) => void>();
  const detach = scope.configure(() => ({
    call: async () => { throw new Error("No one-shot read"); },
    watch(operation, emit) { streams.set(operation.name, emit); return () => { streams.delete(operation.name); }; },
  }));
  const first = createPaginationObserver(queryBinding<Definition>(scope, "first", true, undefined, true), cursor => ({ cursor }));
  const second = createPaginationObserver(queryBinding<Definition>(scope, "second", true, undefined, true), cursor => ({ cursor }));
  const seen: unknown[] = [];
  const stopFirst = first.subscribe(() => {
    if (first.getSnapshot().status === "success") seen.push(second.getSnapshot().data);
  });
  const stopSecond = second.subscribe(() => {});
  try {
    await Promise.resolve();
    for (const label of ["initial", "refreshed"]) batchNotifications(() => {
      for (const stream of streams.values()) {
        const state: QueryState<unknown> = { status: "success", data: { page: [label], cursor: null }, isStale: false, isOptimistic: false };
        notifyObserver(stream, () => stream(state));
      }
    });
    expect(seen).toEqual([["initial"], ["refreshed"]]);
  } finally { stopFirst(); stopSecond(); detach(); }
});

test("paginated seeds and appended ranges retire on auth changes and reject late account deliveries", async () => {
  const { createPaginationObserver } = await import("../src/bindings.ts");
  type Definition = { kind: "query"; $types: { input: { cursor: string | null }; output: import("../src/page.ts").Page<string> } };
  const scope = createRuntimeScope();
  const runtime = new HttpRuntime({ baseUrl: "http://localhost:3001", auth: { key: "alice" }, contracts: {} });
  const streams: { emit: (state: QueryState<unknown>) => void; active: boolean }[] = [];
  const watch = spyOn(runtime, "watch").mockImplementation((_operation, emit) => {
    const stream = { emit, active: true }; streams.push(stream);
    emit({ status: "pending" });
    return () => { stream.active = false; };
  });
  const detach = scope.configure(() => runtime);
  const view = createPaginationObserver(queryBinding<Definition>(scope, "pages", true, undefined, true), cursor => ({ cursor }), { initialData: ["Alice SSR"], initialAuthKey: "alice", keepPreviousDataOnError: true });
  const stop = view.subscribe(() => {});
  try {
    await Promise.resolve();
    streams[0]!.emit({ status: "success", data: { page: ["Alice"], cursor: "second" }, isStale: false, isOptimistic: false });
    await new Promise(resolve => setTimeout(resolve, 0));
    const append = view.getSnapshot().next();
    await Promise.resolve();
    streams[1]!.emit({ status: "success", data: { page: ["Alice second"], cursor: null }, isStale: false, isOptimistic: false });
    await append;
    expect(view.getSnapshot().data).toEqual(["Alice", "Alice second"]);
    let resolve!: (auth: { key: string }) => void;
    const updating = scope.setAuth(new Promise<{ key: string }>(ready => { resolve = ready; }));
    expect(view.getSnapshot().data).toBeUndefined();
    expect(streams.every(stream => !stream.active)).toBe(true);
    resolve({ key: "bob" }); await updating; await Promise.resolve();
    expect(streams).toHaveLength(3);
    streams[0]!.emit({ status: "success", data: { page: ["Alice late"], cursor: null }, isStale: false, isOptimistic: false });
    expect(view.getSnapshot().data).toBeUndefined();
    streams[2]!.emit({ status: "success", data: { page: ["Bob"], cursor: null }, isStale: false, isOptimistic: false });
    expect(view.getSnapshot().data).toEqual(["Bob"]);
    scope.setAuth({ key: "alice" });
    expect(view.getSnapshot().data).toBeUndefined();
  } finally { stop(); detach(); watch.mockRestore(); await runtime.dispose(); }
});
