import { afterAll, beforeAll, expect, test } from "bun:test";
import { plugin } from "bun";
import { compile } from "svelte/compiler";
import { Window } from "happy-dom";
import { createRuntimeScope } from "../../db/src/runtime.ts";
import { mutationBinding, queryBinding } from "../../db/src/callable.ts";
import { type FunctionRuntime, type Operation } from "../../db/src/contracts.ts";
import { type QueryState } from "@hulla/db";
import type { PaginationSnapshot } from "@hulla/db/bindings";
import type { MutationResult } from "../src/index.ts";
import type { SaveMutation } from "./fixture-types.ts";

let browser: Window;
let client: typeof import("svelte");
const originalGlobals = new Map<string, PropertyDescriptor | undefined>();
beforeAll(async () => {
  browser = new Window();
  for (const name of ["window", "document", "navigator", "HTMLElement", "Element", "Node", "Text", "Comment", "Event", "MutationObserver"]) {
    originalGlobals.set(name, Object.getOwnPropertyDescriptor(globalThis, name));
    Object.defineProperty(globalThis, name, { configurable: true, writable: true, value: Reflect.get(browser, name) });
  }
  plugin({ name: "svelte-client-fixtures", setup(build) {
    build.onLoad({ filter: /\.svelte$/ }, async ({ path }) => ({
      contents: compile(await Bun.file(path).text(), { generate: "client", filename: path }).js.code, loader: "js",
    }));
  } });
  client = await import("svelte");
});
afterAll(async () => {
  await browser.happyDOM.close();
  for (const [name, descriptor] of originalGlobals) {
    if (descriptor) Object.defineProperty(globalThis, name, descriptor);
    else Reflect.deleteProperty(globalThis, name);
  }
});

test("compiled components rebind rune inputs, render external snapshots and clean up after unmount", async () => {
  const scope = createRuntimeScope();
  const watches: { operation: Operation; emit: (state: QueryState<unknown>) => void; active: boolean }[] = [];
  let resolveWrite!: (value: unknown) => void;
  const runtime: FunctionRuntime = {
    call(operation) {
      if (operation.kind === "query") throw new Error("Queries must subscribe");
      return new Promise(resolve => { resolveWrite = resolve; });
    },
    watch(operation, emit) {
      const watch = { operation, emit, active: true }; watches.push(watch);
      emit({ status: "pending" }); return () => { watch.active = false; };
    },
  };
  const cleanup = scope.configure(() => runtime);
  const query = queryBinding<{ kind: "query"; $types: { input: { id: number }; output: string } }>(scope, "title", true);
  const mutation = mutationBinding<{ kind: "mutation"; $types: { input: string; output: string } }>(scope, "save", true);
  const { default: Fixture } = await import("./Fixture.svelte");
  const container = document.createElement("div"); document.body.append(container);
  let result!: QueryState<string>;
  let save!: MutationResult<SaveMutation>;
  let setId!: (id: number) => void;
  const component = client.mount(Fixture, { target: container, props: { query, mutation, seed: "server", ready(state: QueryState<string>, write: MutationResult<SaveMutation>, update: (id: number) => void) {
    result = state; save = write; setId = update;
    expect(watches).toHaveLength(0);
  } } });
  try {
    client.flushSync(); await client.tick();
    expect(watches).toHaveLength(1);
    expect(container.textContent).toContain("pending:server");
    watches[0]!.emit({ status: "success", data: "one", isStale: false, isOptimistic: false });
    client.flushSync();
    expect(container.textContent).toContain("success:one");
    setId(1); client.flushSync(); await client.tick();
    expect(watches).toHaveLength(1);
    expect(result.data).toBe("one");
    setId(2); client.flushSync(); await client.tick();
    expect(watches[0]!.active).toBe(false);
    expect(watches[1]!.operation.input).toEqual({ id: 2 });
    expect(result.data).toBeUndefined();
    watches[0]!.emit({ status: "success", data: "late", isStale: false, isOptimistic: false });
    expect(result.data).toBeUndefined();
    setId(1); client.flushSync(); await client.tick();
    expect(result.data).toBeUndefined();
    const handle = save("new"); client.flushSync();
    expect(container.querySelector("span")!.textContent).toBe("pending");
    resolveWrite("saved"); expect(await handle).toBe("saved"); client.flushSync();
    expect(container.querySelector("span")!.textContent).toBe("success");
  } finally { await client.unmount(component); container.remove(); cleanup(); }
  expect(watches.every(watch => !watch.active)).toBe(true);
});


test("compiled paginated components render append progress and rebind rune filters with a stable next delegate", async () => {
  const scope = createRuntimeScope();
  const watches: { operation: Operation; emit: (state: QueryState<unknown>) => void; active: boolean }[] = [];
  const cleanup = scope.configure(() => ({
    call() { throw new Error("Queries must observe"); },
    watch(operation, emit) {
      const watch = { operation, emit, active: true }; watches.push(watch);
      emit({ status: "pending" }); return () => { watch.active = false; };
    },
  }));
  const query = queryBinding<{ kind: "query"; $types: { input: { filter: string; cursor: string | null }; output: import("@hulla/db").Page<{ id: number }> } }>(scope, "tasks", true, undefined, true);
  const { default: Fixture } = await import("./PaginationFixture.svelte");
  const container = document.createElement("div"); document.body.append(container);
  let result!: PaginationSnapshot<{ readonly id: number }>;
  let setFilter!: (filter: string) => void;
  let tick!: () => void;
  const component = client.mount(Fixture, { target: container, props: { query, seed: [{ id: 0 }], ready(state: PaginationSnapshot<{ readonly id: number }>, update: (filter: string) => void, invalidate: () => void) {
    result = state; setFilter = update; tick = invalidate;
    expect(watches).toHaveLength(0);
  } } });
  const emit = (index: number, ids: number[], cursor: string | null) => watches[index]!.emit({ status: "success", data: { page: ids.map(id => ({ id })), cursor }, isStale: false, isOptimistic: false });
  const flush = async () => { client.flushSync(); await client.tick(); await Bun.sleep(0); client.flushSync(); };
  try {
    await flush();
    const next = result.next;
    expect(container.textContent).toContain("pending:0:false");
    emit(0, [1], "second"); await flush();
    expect(container.textContent).toContain("success:1:true");
    tick(); await flush();
    expect(watches).toHaveLength(1);
    const append = next(); await flush();
    expect(watches[1]!.operation.input).toEqual({ filter: "open", cursor: "second" });
    expect(container.textContent).toContain("pending:1:");
    emit(1, [2], null); await append; await flush();
    expect(container.textContent).toContain("success:1,2:false");
    setFilter("closed"); await flush();
    expect(watches.slice(0, 2).every(watch => !watch.active)).toBe(true);
    expect(watches[2]!.operation.input).toEqual({ filter: "closed", cursor: null });
    expect(result.data).toBeUndefined();
    emit(0, [99], null); await flush();
    expect(result.data).toBeUndefined();
    emit(2, [3], "closed-second"); await flush();
    const nextFilterPage = next(); await flush();
    expect(watches[3]!.operation.input).toEqual({ filter: "closed", cursor: "closed-second" });
    emit(3, [4], null); await nextFilterPage; await flush();
    expect(container.textContent).toContain("success:3,4:false");
    setFilter("open"); await flush();
    expect(result.data).toBeUndefined();
  } finally { await client.unmount(component); container.remove(); cleanup(); }
  expect(watches.every(watch => !watch.active)).toBe(true);
});
