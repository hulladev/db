import { AsyncLocalStorage } from "node:async_hooks";
import { afterEach, beforeEach, expect, test } from "bun:test";
import { createRuntimeScope } from "../src/runtime.ts";
import { queryBinding } from "../src/callable.ts";
import { type FunctionRuntime } from "../src/contracts.ts";
import { type QueryState, type MutationState } from "../src/index.ts";
import { storageFixture } from "./fixture.ts";
import { create, pending } from "./generated/notes.ts";
import { runtime as scope } from "./generated/runtime.ts";
import type { Note, ReadContext, WriteContext } from "./fixtures/server.ts";
import { defineFunctions, executeDefinition } from "../src/declarations.ts";
import { standard } from "./fixtures/standard.ts";

const requests = new AsyncLocalStorage<FunctionRuntime>();
let fixture: ReturnType<typeof storageFixture>;
let dispose: () => void;

beforeEach(() => {
  fixture = storageFixture();
  dispose = scope.configure(() => {
    const current = requests.getStore();
    if (!current) throw new Error("No request scope");
    return current;
  });
});
afterEach(() => {
  dispose();
  fixture.close();
});

test("generated direct calls validate inputs, execute business rules and retain typed results", async () => {
  await requests.run(fixture.runtime("alice"), async () => {
    const note = await create({ title: "  Example  " });
    expect(note.title).toBe("Example");
    expect(note.owner).toBe("alice");
    expect(await pending()).toEqual([note]);
    await expect(Promise.resolve(create({ title: "" }))).rejects.toThrow("Title required");
    await expect(Promise.resolve(create({ title: 3 } as unknown as { title: string }))).rejects.toThrow(
      "Expected a string",
    );
    await expect(Promise.resolve(create({ title: "bad", owner: "bob" } as { title: string }))).rejects.toThrow(
      "Unknown input field",
    );
    expect(await pending()).toEqual([note]);
  });
  await requests.run(fixture.runtime(null), async () => {
    await expect(Promise.resolve(create({ title: "Forbidden" }))).rejects.toThrow("Sign in required");
    await expect(Promise.resolve(pending())).rejects.toThrow("Sign in required");
  });
});

test("callables reject mutation cancellation and unknown query options", async () => {
  const runtime = fixture.runtime("alice");
  await requests.run(runtime, async () => {
    await expect(Promise.resolve((create as (...args: unknown[]) => PromiseLike<unknown>)({ title: "Aborted" }, { signal: AbortSignal.abort() }))).rejects.toThrow();
    expect(await pending()).toEqual([]);
    await expect(Promise.resolve(pending({ unknown: true } as unknown as {}))).rejects.toThrow(
      "Unknown call option",
    );
  });
});

test("mutation handles start concurrently, reuse one outcome and recover without predicted output", async () => {
  const submissions: { resolve: (value: Note) => void; recovering: () => void }[] = [];
  const runtime: FunctionRuntime = {
    call(_operation, _options, onStatus) {
      return new Promise(resolve => submissions.push({ resolve, recovering: () => onStatus?.("recovering") }));
    },
  };
  await requests.run(runtime, async () => {
    const first = create({ title: "first" });
    const second = create({ title: "second" });
    expect(submissions).toHaveLength(2);
    expect(first.status).toBe("pending");
    expect(first.data).toBeUndefined();
    const states: MutationState<Note>[] = [];
    const stopBroken = first.subscribe(() => { throw new Error("broken listener"); });
    const stop = first.subscribe(state => states.push(state));
    submissions[0]!.recovering();
    expect(first.status).toBe("recovering");
    expect(first.data).toBeUndefined();
    const note = { id: 1, title: "server normalized", owner: "alice" };
    submissions[0]!.resolve(note);
    const [a, b] = await Promise.all([first, first]);
    expect(a).toBe(b);
    expect(first.data).toBe(a);
    expect(states.map(state => state.status)).toEqual(["pending", "recovering", "success"]);
    submissions[0]!.recovering();
    expect(first.status).toBe("success");
    stop(); stopBroken();
    submissions[1]!.resolve({ ...note, id: 2 });
    await second;
    expect(await first).toBe(a);
    expect(submissions).toHaveLength(2);
  });
});

test("unobserved rejected handles retain their error and later awaits reject the same invocation", async () => {
  const failure = new Error("execution failed");
  let executions = 0;
  await requests.run({ call() { executions++; return Promise.reject(failure); } }, async () => {
    const handle = create({ title: "fails" });
    await new Promise(resolve => setTimeout(resolve, 0));
    expect(handle.status).toBe("error");
    expect(handle.error).toBe(failure);
    expect(handle.data).toBeUndefined();
    const states: MutationState<Note>[] = [];
    const stop = handle.subscribe(state => states.push(state));
    expect(states).toEqual([{ status: "error", error: failure }]);
    await expect(Promise.resolve(handle)).rejects.toBe(failure);
    await expect(Promise.resolve(handle)).rejects.toBe(failure);
    stop();
    expect(executions).toBe(1);
  });
});

test("await-only query handles perform one read and expose readonly shared results", async () => {
  let reads = 0;
  let watches = 0;
  const query = queryBinding<{ kind: "query"; $types: { input: undefined; output: { nested: { values: number[] }; when: Date } } }>(scope, "custom", false);
  await requests.run({
    call() { reads++; return Promise.resolve({ nested: { values: [1] }, when: new Date(0) }); },
    watch() { watches++; return () => {}; },
  }, async () => {
    const handle = query();
    expect(reads).toBe(1);
    expect(handle.status).toBe("pending");
    const result = await handle;
    expect(await handle).toBe(result);
    expect(handle.data).toBe(result);
    expect(watches).toBe(0);
    expect(() => (result.nested.values as number[]).push(2)).toThrow();
    expect(result.when.getTime()).toBe(0);
    expect(handle.isStale).toBe(false);
    expect(handle.isOptimistic).toBe(false);
  });
});

test("direct query subscriptions use the live read without dispatching a duplicate one-shot", async () => {
  let reads = 0;
  let watches = 0;
  let stopped = 0;
  await requests.run({
    call() { reads++; return Promise.resolve([]); },
    watch(_operation, observer) {
      watches++;
      observer({ status: "pending" });
      observer({ status: "success", data: [], isStale: false, isOptimistic: false });
      return () => { stopped++; };
    },
  }, async () => {
    const states: QueryState<Note[]>[] = [];
    const stop = pending.subscribe(state => states.push(state));
    expect(states.map(state => state.status)).toEqual(["pending", "success"]);
    expect(reads).toBe(0);
    expect(watches).toBe(1);
    stop();
    expect(stopped).toBe(1);
  });
});

test("query handle listeners share a live attachment and one-shot completion cannot overwrite a newer live view", async () => {
  let resolve!: (value: Note[]) => void;
  let publish!: (state: QueryState<unknown>) => void;
  let watches = 0;
  let stopped = 0;
  await requests.run({
    call() { return new Promise(done => { resolve = done; }); },
    watch(_operation, observer) { watches++; publish = observer; return () => { stopped++; }; },
  }, async () => {
    const handle = pending();
    const first: QueryState<Note[]>[] = [];
    const second: QueryState<Note[]>[] = [];
    const stopFirst = handle.subscribe(state => first.push(state));
    const abort = new AbortController();
    handle.subscribe(state => second.push(state), { signal: abort.signal });
    expect(watches).toBe(1);
    const latest = [{ id: 2, title: "latest", owner: "alice" }];
    publish({ status: "success", data: latest, isStale: false, isOptimistic: false });
    resolve([{ id: 1, title: "original read", owner: "alice" }]);
    expect((await handle)[0]!.title).toBe("original read");
    expect(handle.data?.[0]!.title).toBe("latest");
    abort.abort();
    expect(stopped).toBe(0);
    publish({ status: "success", data: latest, isStale: true, isOptimistic: false });
    expect(first.at(-1)?.isStale).toBe(true);
    expect(second.at(-1)?.isStale).toBe(false);
    stopFirst();
    expect(stopped).toBe(1);
  });
});

test("query error retention is local to each subscriber and access failure clears retained data", async () => {
  const subscribers = new Set<(state: QueryState<unknown>) => void>();
  await requests.run({
    call() { return Promise.resolve([]); },
    watch(_operation, observer) { subscribers.add(observer); return () => { subscribers.delete(observer); }; },
  }, async () => {
    const clearing: QueryState<Note[]>[] = [];
    const retaining: QueryState<Note[]>[] = [];
    const stopA = pending.subscribe(state => clearing.push(state));
    const stopB = pending.subscribe(state => retaining.push(state), { keepPreviousDataOnError: true });
    const data = [{ id: 1, title: "private", owner: "alice" }];
    for (const publish of subscribers) publish({ status: "success", data, isStale: false, isOptimistic: false });
    const failure = new Error("query failed");
    for (const publish of subscribers) publish({ status: "error", error: failure });
    expect(clearing.at(-1)).toEqual({ status: "error", error: failure });
    expect(retaining.at(-1)).toEqual({ status: "error", error: failure, data, isStale: true, isOptimistic: false });
    const denied = Object.assign(new Error("access revoked"), { code: "forbidden" });
    for (const publish of subscribers) publish({ status: "error", error: denied });
    expect(retaining.at(-1)).toEqual({ status: "error", error: denied });
    // A later handler error cannot resurrect the result from before access was lost.
    for (const publish of subscribers) publish({ status: "error", error: failure });
    expect(retaining.at(-1)).toEqual({ status: "error", error: failure });
    stopA(); stopB();
  });
});

test("query handle publication fences a reentrant access failure before another observer receives retired data", async () => {
  let publish!: (state: QueryState<unknown>) => void;
  await requests.run({
    call() { return Promise.resolve([]); },
    watch(_operation, observer) { publish = observer; return () => {}; },
  }, async () => {
    const handle = pending();
    await handle;
    const denied = Object.assign(new Error("access changed"), { code: "forbidden" });
    let revoke = false;
    const stopA = handle.subscribe(state => {
      if (revoke && state.status === "success") publish({ status: "error", error: denied });
    });
    const states: QueryState<Note[]>[] = [];
    const stopB = handle.subscribe(state => states.push(state));
    revoke = true;
    publish({ status: "success", data: [{ id: 1, title: "retired", owner: "alice" }], isStale: false, isOptimistic: false });
    expect(states.at(-1)).toEqual({ status: "error", error: denied });
    expect(states.some(state => state.status === "success" && state.data.some(note => note.title === "retired"))).toBe(false);
    expect(handle.data).toBeUndefined();
    stopA(); stopB();
  });
});

test("settled query handles retire on identity change before replaying data or attaching a new live scope", async () => {
  let epoch = 1;
  let watches = 0;
  await requests.run({
    get identityEpoch() { return epoch; },
    call() { return Promise.resolve([{ id: 1, title: "private", owner: "alice" }]); },
    watch() { watches++; return () => {}; },
  }, async () => {
    const handle = pending();
    expect((await handle)[0]!.owner).toBe("alice");
    epoch++;
    const states: QueryState<Note[]>[] = [];
    handle.subscribe(state => states.push(state), { keepPreviousDataOnError: true });
    expect(states).toHaveLength(1);
    expect(states[0]?.status).toBe("error");
    expect(handle.data).toBeUndefined();
    expect(handle.status).toBe("error");
    expect(handle.error).toMatchObject({ code: "context_changed" });
    await expect(Promise.resolve(handle)).rejects.toMatchObject({ code: "context_changed" });
    expect(watches).toBe(0);
  });
  await requests.run({
    get identityEpoch() { return epoch; },
    call() { return Promise.resolve([]); },
    watch() { watches++; return () => {}; },
  }, async () => {
    const handle = pending();
    await handle;
    handle.subscribe(state => { if (state.status === "success") epoch++; });
    expect(handle.status).toBe("error");
    expect(watches).toBe(0);
  });
});

test("settled mutation handles retire before replay after identity changes", async () => {
  let epoch = 1;
  await requests.run({
    get identityEpoch() { return epoch; },
    call() { return Promise.resolve({ id: 1, title: "private", owner: "alice" }); },
  }, async () => {
    const handle = create({ title: "private" });
    expect((await handle).owner).toBe("alice");
    epoch++;
    const states: MutationState<Note>[] = [];
    handle.subscribe(state => states.push(state));
    expect(states).toHaveLength(1);
    expect(states[0]).toMatchObject({ status: "error", error: { code: "context_changed" } });
    expect(handle.data).toBeUndefined();
    expect(handle.status).toBe("error");
    expect(handle.error).toMatchObject({ code: "context_changed" });
    await expect(Promise.resolve(handle)).rejects.toMatchObject({ code: "context_changed" });
  });
});

test("observed completed handles clear synchronously when their auth scope is retired", async () => {
  const lifetime = new AbortController();
  let watches = 0;
  let stopped = 0;
  await requests.run({
    get identitySignal() { return lifetime.signal; },
    call(operation) {
      const note = { id: 1, title: "private", owner: "alice" };
      return Promise.resolve(operation.kind === "query" ? [note] : note);
    },
    watch(_operation, observer) {
      watches++;
      observer({ status: "success", data: [{ id: 1, title: "private", owner: "alice" }], isStale: false, isOptimistic: false });
      return () => { stopped++; };
    },
  }, async () => {
    const mutation = create({ title: "private" });
    const query = pending();
    await mutation;
    await query;
    const mutationStates: MutationState<Note>[] = [];
    const queryStates: QueryState<Note[]>[] = [];
    mutation.subscribe(state => mutationStates.push(state));
    query.subscribe(state => {
      queryStates.push(state);
      // Reentrant subscription must not reattach a retired scope.
      if (state.status === "error") query.subscribe(() => {});
    }, { keepPreviousDataOnError: true });
    expect(mutationStates.at(-1)?.status).toBe("success");
    expect(queryStates.at(-1)?.status).toBe("success");
    lifetime.abort();
    // No handle reads or awaits trigger these updates: the observer UI is already cleared.
    expect(mutationStates.at(-1)).toMatchObject({ status: "error", error: { code: "context_changed" } });
    expect(queryStates.at(-1)).toMatchObject({ status: "error", error: { code: "context_changed" } });
    expect(mutationStates.at(-1)).not.toHaveProperty("data");
    expect(queryStates.at(-1)).not.toHaveProperty("data");
    expect(watches).toBe(1);
    expect(stopped).toBe(1);
  });
});

test("auth-scope observation follows the last handle subscription and supports later reattachment", async () => {
  let epoch = 1;
  const lifetime = new AbortController();
  let registrations = 0;
  const attached = new Set<EventListenerOrEventListenerObject>();
  const add = lifetime.signal.addEventListener.bind(lifetime.signal);
  const remove = lifetime.signal.removeEventListener.bind(lifetime.signal);
  lifetime.signal.addEventListener = (type: string, listener: EventListenerOrEventListenerObject | null, options?: boolean | AddEventListenerOptions) => {
    if (type === "abort" && listener) { attached.add(listener); registrations++; }
    if (listener) add(type, listener, options);
  };
  lifetime.signal.removeEventListener = (type: string, listener: EventListenerOrEventListenerObject | null, options?: boolean | EventListenerOptions) => {
    if (type === "abort" && listener) attached.delete(listener);
    if (listener) remove(type, listener, options);
  };
  await requests.run({
    get identityEpoch() { return epoch; },
    get identitySignal() { return lifetime.signal; },
    call() { return Promise.resolve({ id: 1, title: "private", owner: "alice" }); },
  }, async () => {
    const handle = create({ title: "private" });
    await handle;
    expect(attached.size).toBe(0);
    const stopA = handle.subscribe(() => {});
    const stopB = handle.subscribe(() => {});
    expect(attached.size).toBe(1);
    stopA();
    expect(attached.size).toBe(1);
    stopB();
    expect(attached.size).toBe(0);
    const states: MutationState<Note>[] = [];
    handle.subscribe(state => states.push(state));
    expect(attached.size).toBe(1);
    expect(registrations).toBe(2);
    epoch++;
    lifetime.abort();
    expect(states.at(-1)).toMatchObject({ status: "error", error: { code: "context_changed" } });
    expect(attached.size).toBe(0);
  });
});

test("mutation recovery and late outcomes cannot cross an identity change", async () => {
  for (const fail of [false, true]) {
    let epoch = 1;
    let finish!: () => void;
    let recovering!: () => void;
    await requests.run({
      get identityEpoch() { return epoch; },
      call(_operation, _options, onStatus) {
        recovering = () => onStatus?.("recovering");
        return new Promise((resolve, reject) => {
          finish = () => fail
            ? reject(new Error("private failure"))
            : resolve({ id: 1, title: "private", owner: "alice" });
        });
      },
    }, async () => {
      const handle = create({ title: "private" });
      const states: MutationState<Note>[] = [];
      handle.subscribe(state => states.push(state));
      const outcome = Promise.resolve(handle);
      recovering();
      expect(handle.status).toBe("recovering");
      epoch++;
      recovering();
      finish();
      await expect(outcome).rejects.toMatchObject({ code: "context_changed" });
      expect(states.map(state => state.status)).toEqual(["pending", "recovering", "error"]);
      expect(handle.error).toMatchObject({ code: "context_changed" });
      expect(handle.data).toBeUndefined();
    });
  }
});

test("query publication stops at an observer-triggered identity change without a runtime error event", async () => {
  let epoch = 1;
  let publish!: (state: QueryState<unknown>) => void;
  let stopped = 0;
  await requests.run({
    get identityEpoch() { return epoch; },
    call() { return Promise.resolve([]); },
    watch(_operation, observer) { publish = observer; return () => { stopped++; }; },
  }, async () => {
    const handle = pending();
    await handle;
    let revoke = false;
    handle.subscribe(state => { if (revoke && state.status === "success") epoch++; });
    const states: QueryState<Note[]>[] = [];
    handle.subscribe(state => states.push(state), { keepPreviousDataOnError: true });
    revoke = true;
    publish({ status: "success", data: [{ id: 1, title: "private", owner: "alice" }], isStale: false, isOptimistic: false });
    expect(states.at(-1)).toMatchObject({ status: "error", error: { code: "context_changed" } });
    expect(states.some(state => "data" in state && state.data?.some(note => note.title === "private"))).toBe(false);
    expect(handle.data).toBeUndefined();
    expect(stopped).toBe(1);
  });
});

test("mutation publication fences remaining observers and awaiting callers after reentrant identity change", async () => {
  let epoch = 1;
  await requests.run({
    get identityEpoch() { return epoch; },
    call() { return Promise.resolve({ id: 1, title: "private", owner: "alice" }); },
  }, async () => {
    const handle = create({ title: "private" });
    handle.subscribe(state => { if (state.status === "success") epoch++; });
    const states: MutationState<Note>[] = [];
    handle.subscribe(state => states.push(state));
    await expect(Promise.resolve(handle)).rejects.toMatchObject({ code: "context_changed" });
    expect(states.map(state => state.status)).toEqual(["pending", "error"]);
    expect(handle.data).toBeUndefined();
  });
});

test("optimistic wrappers map exact and matching cached queries while preserving authoritative mutation results", async () => {
  const filtered = queryBinding<{ kind: "query"; $types: { input: { owner: string }; output: Note[] } }>(scope, "custom.filtered", true);
  const unrelated = queryBinding<{ kind: "query"; $types: { input: undefined; output: Note[] } }>(createRuntimeScope(), "notes.pending", false);
  expect(() => create.withOptimistic({ query: unrelated, update: current => current })).toThrow("Optimistic targets");
  expect(() => create.withOptimistic({ query: filtered, update: (current: Note[]) => current } as never)).toThrow("input mapper or match");
  const data = new Map<string, unknown>([
    ["notes.pending", []],
    ["custom.filtered:alice", []],
    ["custom.filtered:bob", []],
  ]);
  const key = (query: { name: string; input?: unknown }) => query.name + (query.input ? ":" + (query.input as { owner: string }).owner : "");
  const optimistic = create.withOptimistic({
    query: pending,
    update: (current, input) => [...current, { id: -1, owner: "alice", title: input.title }],
  }, {
    query: filtered,
    match: (queryInput, mutationInput) => queryInput.owner === "alice" && mutationInput.title.length > 0,
    update: (current, input, queryInput) => [...current, { id: -1, owner: queryInput.owner, title: input.title }],
  }, {
    query: filtered,
    input: () => ({ owner: "bob" }),
    update: (current, input, queryInput) => [...current, { id: -1, owner: queryInput.owner, title: `exact: ${input.title}` }],
  });
  let baseWasOptimistic = true;
  await requests.run({
    call(operation, options) {
      if (options.optimistic) options.optimistic(operation.input, {
        requestId: "request",
        get: query => data.get(key(query)),
        set: (query, value) => { data.set(key(query), value); },
        queries: name => name === "custom.filtered" ? [
          { name, input: { owner: "alice" } }, { name, input: { owner: "bob" } },
        ] : name === "notes.pending" && data.has(name) ? [{ name, input: undefined }] : [],
      });
      else baseWasOptimistic = false;
      return Promise.resolve({ id: 1, owner: "alice", title: "server" });
    },
  }, async () => {
    const handle = optimistic({ title: "predicted" });
    expect(handle.data).toBeUndefined();
    expect(data.get("notes.pending")).toEqual([{ id: -1, owner: "alice", title: "predicted" }]);
    expect(data.get("custom.filtered:alice")).toEqual([{ id: -1, owner: "alice", title: "predicted" }]);
    expect(data.get("custom.filtered:bob")).toEqual([{ id: -1, owner: "bob", title: "exact: predicted" }]);
    expect((await handle).title).toBe("server");
    expect(handle.data?.title).toBe("server");
    data.delete("notes.pending");
    await optimistic({ title: "missing cached query" });
    expect(data.has("notes.pending")).toBe(false);
    await create({ title: "base" });
    expect(baseWasOptimistic).toBe(false);
  });
});

test("aborting an initial query clears an already delivered live view and releases its attachment", async () => {
  let publish!: (state: QueryState<unknown>) => void;
  let stopped = 0;
  await requests.run({
    call(_operation, options) {
      return new Promise((_resolve, reject) => options.signal?.addEventListener("abort", () => reject(options.signal!.reason), { once: true }));
    },
    watch(_operation, observer) { publish = observer; return () => { stopped++; }; },
  }, async () => {
    const abort = new AbortController();
    const handle = pending({ signal: abort.signal });
    const states: QueryState<Note[]>[] = [];
    const stop = handle.subscribe(state => states.push(state));
    publish({ status: "success", data: [{ id: 1, title: "live", owner: "alice" }], isStale: false, isOptimistic: false });
    const failure = new Error("query cancelled");
    abort.abort(failure);
    await expect(Promise.resolve(handle)).rejects.toBe(failure);
    expect(handle.status).toBe("error");
    expect(handle.data).toBeUndefined();
    expect(handle.error).toBe(failure);
    expect(stopped).toBe(1);
    publish({ status: "success", data: [], isStale: false, isOptimistic: false });
    expect(states.at(-1)).toEqual({ status: "error", error: failure });
    stop();
    expect(stopped).toBe(1);
  });
});

test("Standard Schema input and output transforms run asynchronously with raw caller and parsed handler values", async () => {
  const { query } = defineFunctions<{}, {}>();
  const converted = query({
    input: standard<string, number>(async value => {
      if (typeof value !== "string") throw new TypeError("Expected explicit string input");
      return Number(value);
    }),
    output: standard<number, { doubled: number }>(async value => {
      if (typeof value !== "number") throw new TypeError("Expected number output");
      return { doubled: value * 2 };
    }),
    handler: async (_ctx, parsed) => parsed + 1,
  });
  expect(converted.hasInput).toBe(true);
  expect(await executeDefinition(converted, {}, "20")).toEqual({ doubled: 42 });
  await expect(executeDefinition(converted, {}, 20)).rejects.toThrow("Expected explicit string input");
  const noInput = query({ handler: () => "ready" });
  expect(await executeDefinition(noInput, {}, undefined)).toBe("ready");
  await expect(executeDefinition(noInput, {}, {})).rejects.toThrow("takes no input");
});

test("extensions validate input immediately before each stage and stop after a denied guard", async () => {
  const { query } = defineFunctions<{ identity: string | null; permissionRead(): Promise<boolean> }, {}>();
  const events: string[] = [];
  const guarded = query.extend({
    input: standard<{ project: string }, { project: string }>(value => {
      events.push("guard input");
      return { project: (value as { project: string }).project.toUpperCase() };
    }),
    handler: async (ctx, input) => {
      events.push("guard");
      if (!ctx.identity || !await ctx.permissionRead()) throw new Error("Denied");
      return { authorizedProject: input.project };
    },
  }).extend((ctx, input) => {
    events.push("second guard");
    return { tag: `${ctx.authorizedProject}:${input.project}` };
  });
  const definition = guarded({
    input: standard<{ take: string }, { take: number }>(value => {
      events.push("final input");
      if ((value as { project: string }).project !== "project") throw new Error("Original input changed");
      return { take: Number((value as { take: string }).take) };
    }),
    handler: (ctx, input) => { events.push("handler"); return { project: ctx.authorizedProject, tag: ctx.tag, take: input.take, parsedProject: input.project }; },
  });
  let identity: string | null = "alice";
  const execute = (input: unknown) => executeDefinition(definition, {
    identity, async permissionRead() { events.push("permission read"); return true; },
  }, input);
  expect(await execute({ project: "project", take: "2" })).toEqual({ project: "PROJECT", tag: "PROJECT:PROJECT", take: 2, parsedProject: "PROJECT" });
  expect(events).toEqual(["guard input", "guard", "permission read", "second guard", "final input", "handler"]);
  events.length = 0;
  identity = null;
  await expect(execute({ project: "project", take: "invalid" })).rejects.toThrow("Denied");
  expect(events).toEqual(["guard input", "guard"]);
});

test("output rejection rolls back a mutation write and output transformation determines the committed result", async () => {
  const { mutation } = defineFunctions<ReadContext, WriteContext>();
  let outputRuns = 0;
  const output = standard<Note, { title: string }>(async value => {
    outputRuns++;
    await Promise.resolve();
    const note = value as Note;
    if (note.title === "invalid") throw new TypeError("Output validation failed");
    return { title: note.title.toUpperCase() };
  });
  const write = mutation({ output, handler: ctx => ctx.db.insert("invalid", "alice") });
  const transformed = mutation({ output, handler: ctx => ctx.db.insert("valid", "alice") });
  const thrown = mutation({ output, handler: async ctx => { await ctx.db.insert("throw", "alice"); throw new Error("Handler error"); } });
  const storage = storageFixture({ app: { write, transformed, thrown } });
  const runtime = storage.runtime("alice");
  try {
    await expect(runtime.call({ name: "app.write", kind: "mutation", input: undefined }, {})).rejects.toThrow("Output validation failed");
    expect(await runtime.call({ name: "notes.pending", kind: "query", input: undefined }, {})).toEqual([]);
    expect(await runtime.call({ name: "app.transformed", kind: "mutation", input: undefined }, {})).toEqual({ title: "VALID" });
    await expect(runtime.call({ name: "app.thrown", kind: "mutation", input: undefined }, {})).rejects.toThrow("Handler error");
    expect(outputRuns).toBe(2);
    const rows = await runtime.call({ name: "notes.pending", kind: "query", input: undefined }, {}) as Note[];
    expect(rows.map(note => note.title)).toEqual(["valid"]);
  } finally { storage.close(); }
});

test("each input stage gets an independent original value even when a validator and handler mutate their local copy", async () => {
  const { query } = defineFunctions<{}, {}>();
  const original = { nested: { value: "original" }, final: "ok" };
  const definition = query.extend({
    input: standard<{ nested: { value: string } }, { first: { value: string } }>(value => {
      const raw = value as typeof original;
      raw.nested.value = "validator change";
      return { first: raw.nested };
    }),
    handler: (_ctx, input) => { input.first.value = "handler change"; },
  })({
    input: standard<{ final: string }, { second: string }>(value => {
      expect((value as typeof original).nested.value).toBe("original");
      return { second: (value as typeof original).final };
    }),
    handler: (_ctx, input) => ({ first: input.first.value, second: input.second }),
  });
  expect(await executeDefinition(definition, {}, original)).toEqual({ first: "handler change", second: "ok" });
  expect(original.nested.value).toBe("original");
});

test("runtime stages reject dynamic duplicate keys, reserved context, accessors and prototype payloads", async () => {
  const { query } = defineFunctions<{ db: {}; identity: null }, {}>();
  let finalRan = false;
  const duplicateInput = query.extend({
    input: standard<Record<string, unknown>, object>(() => ({ shared: "first" })),
    handler: () => {},
  })({
    input: standard<Record<string, unknown>, object>(() => ({ shared: "second" })),
    handler: () => { finalRan = true; },
  });
  await expect(executeDefinition(duplicateInput, { db: {}, identity: null }, {})).rejects.toThrow("Duplicate parsed input key");
  expect(finalRan).toBe(false);
  const prior = query.extend(() => ({ tag: "first" }));
  const dynamic = () => ({ tag: "second" }) as Record<string, unknown>;
  await expect(executeDefinition(prior.extend(dynamic)(() => "unreachable"), { db: {}, identity: null }, undefined)).rejects.toThrow("Duplicate or reserved context key");
  for (const properties of [
    { db: {} }, { identity: "forged" }, JSON.parse('{"__proto__":{"polluted":true}}'),
    Object.defineProperty({}, "computed", { enumerable: true, get() { throw new Error("Must not invoke accessor"); } }),
  ]) {
    const definition = query.extend(() => properties as Record<string, unknown>)(() => "unreachable");
    await expect(executeDefinition(definition, { db: {}, identity: null }, undefined)).rejects.toBeInstanceOf(TypeError);
  }
  expect(({} as { polluted?: boolean }).polluted).toBeUndefined();
  const incompatible = query.extend({ input: standard<unknown, object>(() => "scalar" as unknown as object), handler: () => {} })({
    input: standard<unknown, object>(() => ({ final: true })), handler: () => "unreachable",
  });
  await expect(executeDefinition(incompatible, { db: {}, identity: null }, {})).rejects.toThrow("Parsed input must be a plain object");
  const authoredLimits = { outputBytes: 32 };
  const limited = query({ limits: authoredLimits, handler: () => "bounded" });
  authoredLimits.outputBytes = 999;
  expect(limited.limits?.outputBytes).toBe(32);
  expect(() => query({ limits: { executionMs: 0 }, handler: () => "invalid" })).toThrow("limits.executionMs");
  expect(() => (query.extend as (...args: unknown[]) => unknown)({ limits: { outputBytes: 32 }, handler: () => ({}) })).toThrow("Unknown function declaration option");
  const noImplicitLimits = query as (...args: unknown[]) => unknown;
  expect(() => noImplicitLimits({ retries: 3, handler: () => "ignored" })).toThrow("Unknown function declaration option");
  expect(() => noImplicitLimits({ input: { parse: () => "old" }, handler: () => "ignored" })).toThrow("Standard Schema");
  expect(() => noImplicitLimits({ title: {} }, () => "old positional form")).toThrow("takes one");
});
