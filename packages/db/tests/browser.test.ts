import { afterEach, expect, spyOn, test } from "bun:test";
import { createBrowserRuntime } from "../src/browser.ts";
import { createRuntimeScope } from "../src/runtime.ts";
import { contracts } from "./generated/runtime.ts";
import { queryBinding } from "../src/callable.ts";

const cleanups: (() => void | Promise<void>)[] = [];
afterEach(async () => { for (const cleanup of cleanups.splice(0).reverse()) await cleanup(); });

test("generated configuration stays lazy, forwards credentials, and retires work on auth change", async () => {
  const requests: RequestInit[] = [];
  let requested!: () => void;
  const started = new Promise<void>(resolve => { requested = resolve; });
  const fetcher = spyOn(globalThis, "fetch").mockImplementation((async (_url, init) => {
    requests.push(init!);
    requested();
    return await new Promise<Response>(() => {});
  }) as typeof fetch);
  cleanups.push(() => { fetcher.mockRestore(); });
  let token = "first";
  const { runtime, setAuth, setHeaders, dispose } = createBrowserRuntime(contracts, { url: "http://localhost:9000", credentials: "include" });
  const pending = queryBinding<typeof import("./fixtures/notes.ts").pending>(runtime, "notes.pending", false, contracts["notes.pending"]);
  setHeaders(() => ({ authorization: `Bearer ${token}` }));
  setAuth({ key: "alice" });
  cleanups.push(dispose);
  expect(fetcher).not.toHaveBeenCalled();
  const call = Promise.resolve(pending()).catch(error => error);
  await started;
  expect(requests[0]?.credentials).toBe("include");
  expect(new Headers(requests[0]?.headers).get("authorization")).toBe("Bearer first");
  token = "second";
  setAuth({ key: "bob" });
  expect(requests[0]?.signal?.aborted).toBe(true);
  expect(await call).toMatchObject({ code: "context_changed" });

  let nextRequested!: () => void;
  const nextStarted = new Promise<void>(resolve => { nextRequested = resolve; });
  requested = nextRequested;
  const nextCall = Promise.resolve(pending()).catch(error => error);
  await nextStarted;
  expect(new Headers(requests[1]?.headers).get("authorization")).toBe("Bearer second");
  const disposal = dispose();
  expect(() => runtime.current()).toThrow("No function runtime");
  expect(dispose()).toBe(disposal);
  await disposal;
  expect(requests[1]?.signal?.aborted).toBe(true);
  expect(await nextCall).toMatchObject({ code: "context_changed" });
});

test("pagination stages sibling lists before observers and fences an already resolved first await on auth retirement", async () => {
  const { queryBinding } = await import("../src/callable.ts");
  const { batchNotifications, notifyObserver } = await import("../src/notifications.ts");
  type Page = import("../src/page.ts").Page<string>;
  type Definition = { kind: "query"; $types: { input: { cursor: string | null }; output: Page } };
  type State = import("../src/contracts.ts").QueryState<unknown>;
  const scope = createRuntimeScope();
  const identity = new AbortController();
  const observers = new Map<string, (state: State) => void>();
  const detach = scope.configure(() => ({
    identitySignal: identity.signal,
    identityEpoch: identity.signal.aborted ? 1 : 0,
    call: async () => { throw new Error("Pagination uses live rounds"); },
    watch(operation, observer) {
      observers.set(operation.name, observer);
      return () => { observers.delete(operation.name); };
    },
  }));
  cleanups.push(detach);
  const first = queryBinding<Definition>(scope, "first", true, undefined, true).paginate(cursor => ({ cursor }));
  const second = queryBinding<Definition>(scope, "second", true, undefined, true).paginate(cursor => ({ cursor }));
  cleanups.push(() => { first.dispose(); second.dispose(); });
  const observed: (readonly string[] | undefined)[] = [];
  first.subscribe(state => { if (state.status === "success") observed.push(second.data); });
  await Promise.resolve();
  const round = (label: string) => batchNotifications(() => {
    for (const observer of observers.values()) {
      const state: State = { status: "success", data: { page: [label], cursor: null }, isStale: false, isOptimistic: false };
      notifyObserver(observer, () => observer(state));
    }
  });
  round("initial");
  await first; await second;
  expect(observed).toEqual([["initial"]]);
  round("updated");
  expect(observed).toEqual([["initial"], ["updated"]]);
  const lateAwait = Promise.resolve(first);
  identity.abort();
  expect(first.data).toBeUndefined();
  expect(second.data).toBeUndefined();
  await expect(lateAwait).rejects.toMatchObject({ code: "context_changed" });
  expect(observers.size).toBe(0);
});

test("a protected loaded-page error wins over a competing append failure even with retention", async () => {
  const { queryBinding } = await import("../src/callable.ts");
  const { DatabaseError } = await import("../src/error.ts");
  const { batchNotifications, notifyObserver } = await import("../src/notifications.ts");
  type Page = import("../src/page.ts").Page<string>;
  type Definition = { kind: "query"; $types: { input: { cursor: string | null }; output: Page } };
  type State = import("../src/contracts.ts").QueryState<unknown>;
  const scope = createRuntimeScope();
  const observers: ((state: State) => void)[] = [];
  const detach = scope.configure(() => ({
    call: async () => { throw new Error("Pagination uses live rounds"); },
    watch(_operation, observer) {
      observers.push(observer);
      return () => {};
    },
  }));
  cleanups.push(detach);
  const list = queryBinding<Definition>(scope, "protected", true, undefined, true).paginate(cursor => ({ cursor }));
  cleanups.push(() => list.dispose());
  const errors: { error: unknown; data: unknown }[] = [];
  list.subscribe(state => {
    if (state.status === "error") errors.push({ error: state.error, data: state.data });
  }, { keepPreviousDataOnError: true });
  await Promise.resolve();
  const initial: State = { status: "success", data: { page: ["private"], cursor: "next" }, isStale: false, isOptimistic: false };
  batchNotifications(() => notifyObserver(observers[0]!, () => observers[0]!(initial)));
  await list;
  const append = list.next();
  await Promise.resolve();
  const forbidden = new DatabaseError({ code: "forbidden", message: "forbidden" });
  const limited = new DatabaseError({ code: "execution_limit", message: "execution_limit" });
  batchNotifications(() => {
    notifyObserver(observers[0]!, () => observers[0]!({ status: "error", error: forbidden }));
    notifyObserver(observers[1]!, () => observers[1]!({ status: "error", error: limited }));
  });
  await expect(append).rejects.toMatchObject({ code: "execution_limit" });
  expect(list.error).toBe(forbidden);
  expect(list.data).toBeUndefined();
  expect(errors.length).toBeGreaterThan(0);
  expect(errors.every(state => state.error === forbidden && state.data === undefined)).toBe(true);
});

test.each([false, true])("aborting pagination in its initial callback releases ownership (watch started=%s)", async (startWatch) => {
  const { queryBinding } = await import("../src/callable.ts");
  type Page = import("../src/page.ts").Page<string>;
  type Definition = { kind: "query"; $types: { input: { cursor: string | null }; output: Page } };
  const scope = createRuntimeScope();
  let started = 0, active = 0;
  const detach = scope.configure(() => ({
    call: async () => { throw new Error("Pagination uses live rounds"); },
    watch() {
      started++; active++;
      return () => { active--; };
    },
  }));
  cleanups.push(detach);
  const list = queryBinding<Definition>(scope, "aborted", true, undefined, true).paginate(cursor => ({ cursor }));
  cleanups.push(() => list.dispose());
  if (startWatch) await Promise.resolve();
  const controller = new AbortController();
  list.subscribe(() => controller.abort(), { signal: controller.signal });
  await expect(Promise.resolve(list)).rejects.toMatchObject({ code: "disposed" });
  expect(started).toBe(startWatch ? 1 : 0);
  expect(active).toBe(0);
});

test("synchronous page delivery releases a watch acquired after its subscriber aborts", async () => {
  const { queryBinding } = await import("../src/callable.ts");
  type Page = import("../src/page.ts").Page<string>;
  type Definition = { kind: "query"; $types: { input: { cursor: string | null }; output: Page } };
  const scope = createRuntimeScope();
  let released = 0;
  const detach = scope.configure(() => ({
    call: async () => { throw new Error("Pagination uses live rounds"); },
    watch(_operation, observer) {
      observer({ status: "success", data: { page: ["result"], cursor: null }, isStale: false, isOptimistic: false });
      return () => { released++; };
    },
  }));
  cleanups.push(detach);
  const list = queryBinding<Definition>(scope, "synchronous", true, undefined, true).paginate(cursor => ({ cursor }));
  cleanups.push(() => list.dispose());
  const controller = new AbortController();
  list.subscribe(state => { if (state.status === "success") controller.abort(); }, { signal: controller.signal });
  await expect(Promise.resolve(list)).rejects.toMatchObject({ code: "disposed" });
  expect(released).toBe(1);
});

test.each([false, true])("pagination fences later subscribers when an earlier observer retires auth (batched=%s)", async (batched) => {
  const { queryBinding } = await import("../src/callable.ts");
  const { batchNotifications, notifyObserver } = await import("../src/notifications.ts");
  type Page = import("../src/page.ts").Page<string>;
  type Definition = { kind: "query"; $types: { input: { cursor: string | null }; output: Page } };
  type State = import("../src/contracts.ts").QueryState<unknown>;
  const scope = createRuntimeScope();
  const identity = new AbortController();
  let observer!: (state: State) => void;
  const detach = scope.configure(() => ({
    identitySignal: identity.signal,
    call: async () => { throw new Error("Pagination uses live rounds"); },
    watch(_operation, next) { observer = next; return () => {}; },
  }));
  cleanups.push(detach);
  const list = queryBinding<Definition>(scope, "auth-fence", true, undefined, true).paginate(cursor => ({ cursor }));
  cleanups.push(() => list.dispose());
  const later: import("../src/contracts.ts").QueryState<readonly string[]>[] = [];
  list.subscribe(state => { if (state.status === "success") identity.abort(); });
  list.subscribe(state => { later.push(state); }, { keepPreviousDataOnError: true });
  await Promise.resolve();
  const emit = () => notifyObserver(observer, () => observer({
    status: "success", data: { page: ["private"], cursor: null }, isStale: false, isOptimistic: false,
  }));
  if (batched) batchNotifications(emit); else emit();
  await expect(Promise.resolve(list)).rejects.toMatchObject({ code: "context_changed" });
  expect(later.some(state => state.status === "success")).toBe(false);
  expect(later.at(-1)).toMatchObject({ status: "error", error: { code: "context_changed" } });
  expect(later.at(-1)?.data).toBeUndefined();
});

test.each(["mapper", "result"])("pagination rejects nonadvancing %s without publishing repeated rows", async (failure) => {
  const { queryBinding } = await import("../src/callable.ts");
  type Page = import("../src/page.ts").Page<string>;
  type Definition = { kind: "query"; $types: { input: { cursor: string | null }; output: Page } };
  type State = import("../src/contracts.ts").QueryState<unknown>;
  const scope = createRuntimeScope();
  const observers: ((state: State) => void)[] = [];
  const detach = scope.configure(() => ({
    call: async () => { throw new Error("Pagination uses live rounds"); },
    watch(_operation, observer) { observers.push(observer); return () => {}; },
  }));
  cleanups.push(detach);
  const list = queryBinding<Definition>(scope, "progress", true, undefined, true).paginate(cursor => ({ cursor: failure === "mapper" ? null : cursor }));
  cleanups.push(() => list.dispose());
  const snapshots: unknown[] = [];
  list.subscribe(state => { if (state.data) snapshots.push(state.data); });
  await Promise.resolve();
  observers[0]!({ status: "success", data: { page: ["first"], cursor: "after-first" }, isStale: false, isOptimistic: false });
  await list;
  const append = list.next();
  if (failure === "result") {
    await Promise.resolve();
    observers[1]!({ status: "success", data: { page: ["first"], cursor: "after-first" }, isStale: false, isOptimistic: false });
  }
  await expect(append).rejects.toBeInstanceOf(TypeError);
  expect(list.data).toEqual(["first"]);
  expect(snapshots.every(rows => Array.isArray(rows) && rows.length === 1 && rows[0] === "first")).toBe(true);
  expect(observers.length).toBe(failure === "mapper" ? 1 : 2);
});
