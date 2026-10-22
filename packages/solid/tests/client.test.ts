import { expect, test } from "bun:test";
import { createRoot, createSignal } from "solid-js";
import { createRuntimeScope } from "../../db/src/runtime.ts";
import { mutationBinding, queryBinding } from "../../db/src/callable.ts";
import { type FunctionRuntime, type Operation } from "../../db/src/contracts.ts";
import { type Page, type QueryState } from "@hulla/db";
import { createMutation, createPaginatedQuery, createQuery } from "../src/index.ts";

test("committed owners observe reactive inputs, retire old watches and never reuse the SSR seed", async () => {
  const scope = createRuntimeScope();
  const watches: { operation: Operation; emit: (state: QueryState<unknown>) => void; active: boolean }[] = [];
  let calls = 0;
  const runtime: FunctionRuntime = {
    call() { calls++; throw new Error("Queries must observe"); },
    watch(operation, emit) {
      const watch = { operation, emit, active: true }; watches.push(watch);
      emit({ status: "pending" }); return () => { watch.active = false; };
    },
  };
  const cleanup = scope.configure(() => runtime);
  const query = queryBinding<{ kind: "query"; $types: { input: { id: number }; output: string } }>(scope, "title", true);
  const [id, setId] = createSignal(1);
  const [tick, setTick] = createSignal(0);
  let state!: ReturnType<typeof createQuery<typeof query>>;
  let dispose!: () => void;
  try {
    createRoot(stop => {
      dispose = stop;
      state = createQuery(query, () => { tick(); return { id: id() }; }, { initialData: "server" });
      expect(watches).toHaveLength(0);
      expect(state().data).toBe("server");
    });
    expect(watches).toHaveLength(1);
    watches[0]!.emit({ status: "success", data: "one", isStale: false, isOptimistic: false });
    expect(state().data).toBe("one");
    setTick(1);
    expect(watches).toHaveLength(1);
    expect(state().data).toBe("one");
    setId(2);
    expect(watches[0]!.active).toBe(false);
    expect(watches[1]!.operation.input).toEqual({ id: 2 });
    expect(state().data).toBeUndefined();
    watches[0]!.emit({ status: "success", data: "late", isStale: false, isOptimistic: false });
    expect(state().data).toBeUndefined();
    setId(1);
    expect(state().data).toBeUndefined();
    expect(calls).toBe(0);
  } finally { dispose?.(); cleanup(); }
  expect(watches.every(watch => !watch.active)).toBe(true);
});

test("mutation state is reactive while each call retains its own handle", async () => {
  const scope = createRuntimeScope();
  const writes: ((value: unknown) => void)[] = [];
  const cleanup = scope.configure(() => ({ call: () => new Promise(resolve => writes.push(resolve)) }));
  const mutation = mutationBinding<{ kind: "mutation"; $types: { input: string; output: string } }>(scope, "save", true);
  let save!: ReturnType<typeof createMutation<typeof mutation>>;
  let dispose!: () => void;
  try {
    createRoot(stop => { dispose = stop; save = createMutation(mutation); });
    expect(save.state().status).toBe("idle");
    const first = save("first");
    const second = save("second");
    expect(save.state().status).toBe("pending");
    writes[1]!("second result"); expect(await second).toBe("second result");
    expect(save.state()).toEqual({ status: "success", data: "second result" });
    writes[0]!("first result"); expect(await first).toBe("first result");
    expect(save.state()).toEqual({ status: "success", data: "second result" });
  } finally { dispose?.(); cleanup(); }
});


test("paginated owners append live ranges and reset filters without reusing seeds or old next delegates", async () => {
  const scope = createRuntimeScope();
  const watches: { operation: Operation; emit: (state: QueryState<unknown>) => void; active: boolean }[] = [];
  const cleanup = scope.configure(() => ({
    call() { throw new Error("Queries must observe"); },
    watch(operation, emit) {
      const watch = { operation, emit, active: true }; watches.push(watch);
      emit({ status: "pending" }); return () => { watch.active = false; };
    },
  }));
  const query = queryBinding<{ kind: "query"; $types: { input: { filter: string; cursor: string | null }; output: Page<{ id: number }> } }>(scope, "tasks", true, undefined, true);
  const [filter, setFilter] = createSignal("open");
  const [tick, setTick] = createSignal(0);
  let state!: ReturnType<typeof createPaginatedQuery<typeof query>>;
  let dispose!: () => void;
  const emit = (index: number, ids: number[], cursor: string | null) => watches[index]!.emit({ status: "success", data: { page: ids.map(id => ({ id })), cursor }, isStale: false, isOptimistic: false });
  try {
    createRoot(stop => {
      dispose = stop;
      state = createPaginatedQuery(query, cursor => { tick(); return { filter: filter(), cursor }; }, { initialData: [{ id: 0 }] });
      expect(watches).toHaveLength(0);
      expect(state().data).toEqual([{ id: 0 }]);
    });
    const next = state.next;
    await Bun.sleep(0);
    emit(0, [1], "second"); await Bun.sleep(0);
    expect(state().hasMore).toBe(true);
    setTick(1);
    expect(watches).toHaveLength(1);
    const append = next(); await Bun.sleep(0);
    expect(watches[1]!.operation.input).toEqual({ filter: "open", cursor: "second" });
    expect(state().status).toBe("pending");
    expect(state().data).toEqual([{ id: 1 }]);
    emit(1, [2], null); await append;
    expect(state().data).toEqual([{ id: 1 }, { id: 2 }]);
    expect(state().hasMore).toBe(false);
    setFilter("closed"); await Bun.sleep(0);
    expect(watches.slice(0, 2).every(watch => !watch.active)).toBe(true);
    expect(watches[2]!.operation.input).toEqual({ filter: "closed", cursor: null });
    expect(state().data).toBeUndefined();
    emit(0, [99], null);
    expect(state().data).toBeUndefined();
    emit(2, [3], "closed-second"); await Bun.sleep(0);
    const nextFilterPage = next(); await Bun.sleep(0);
    expect(watches[3]!.operation.input).toEqual({ filter: "closed", cursor: "closed-second" });
    emit(3, [4], null); await nextFilterPage;
    expect(state().data).toEqual([{ id: 3 }, { id: 4 }]);
    setFilter("open"); await Bun.sleep(0);
    expect(state().data).toBeUndefined();
  } finally { dispose?.(); cleanup(); }
  expect(watches.every(watch => !watch.active)).toBe(true);
});
