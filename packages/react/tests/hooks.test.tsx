import { afterAll, afterEach, beforeAll, beforeEach, expect, test } from "bun:test";
import { act, StrictMode } from "react";
import { renderToString } from "react-dom/server";
import type { Root } from "react-dom/client";
import { Window } from "happy-dom";
import { createRuntimeScope } from "../../db/src/runtime.ts";
import { mutationBinding, queryBinding } from "../../db/src/callable.ts";
import { type FunctionRuntime, type Operation } from "../../db/src/contracts.ts";
import { type MutationHandle, type QueryState } from "@hulla/db";
import { useMutation, useQuery, usePaginatedQuery, type MutationResult } from "../src/index.ts";

let client: typeof import("react-dom/client");
let browser: Window;
let container: HTMLDivElement;
let root: Root | undefined;
const disposers: (() => void)[] = [];
const originalGlobals = new Map<string, PropertyDescriptor | undefined>();

beforeAll(async () => {
  // React selects its browser implementation when react-dom/client is loaded.
  browser = new Window();
  for (const [name, value] of Object.entries({
    window: browser,
    document: browser.document,
    navigator: browser.navigator,
    HTMLElement: browser.HTMLElement,
    Node: browser.Node,
    IS_REACT_ACT_ENVIRONMENT: true,
  })) {
    originalGlobals.set(name, Object.getOwnPropertyDescriptor(globalThis, name));
    Object.defineProperty(globalThis, name, { configurable: true, writable: true, value });
  }
  client = await import("react-dom/client");
});

beforeEach(() => {
  container = document.createElement("div");
  document.body.append(container);
});

afterEach(async () => {
  if (root) await act(() => root!.unmount());
  root = undefined;
  container.remove();
  for (const dispose of disposers.splice(0).reverse()) dispose();
  await browser.happyDOM.abort();
});

afterAll(async () => {
  await browser.happyDOM.close();
  for (const [name, descriptor] of originalGlobals) {
    if (descriptor) Object.defineProperty(globalThis, name, descriptor);
    else Reflect.deleteProperty(globalThis, name);
  }
});

function fixture() {
  const scope = createRuntimeScope();
  const watches: {
    operation: Operation;
    emit: (state: QueryState<unknown>) => void;
    active: boolean;
  }[] = [];
  const writes: {
    resolve: (value: unknown) => void;
    recover: () => void;
  }[] = [];
  let calls = 0;
  const runtime: FunctionRuntime = {
    call(operation, _options, onStatus) {
      calls++;
      if (operation.kind === "query") throw new Error("Hooks must subscribe without a one-shot read");
      return new Promise(resolve => writes.push({ resolve, recover: () => onStatus?.("recovering") }));
    },
    watch(operation, emit) {
      const watch = { operation, emit, active: true };
      watches.push(watch);
      emit({ status: "pending" });
      return () => { watch.active = false; };
    },
  };
  disposers.push(scope.configure(() => runtime));
  return { scope, watches, writes, get calls() { return calls; } };
}

test("SSR keeps request seeds isolated without resolving a runtime or starting reads", () => {
  const query = queryBinding<{ kind: "query"; $types: { input: undefined; output: string } }>(createRuntimeScope(), "title", false);
  function Title({ initialData }: { initialData: string }) {
    const state = useQuery(query, { initialData });
    return <p>{state.status}:{state.data}:{String(state.isStale)}</p>;
  }
  container.innerHTML = renderToString(<Title initialData="first request" />);
  expect(container.textContent).toBe("pending:first request:true");
  container.innerHTML = renderToString(<Title initialData="second request" />);
  expect(container.textContent).toBe("pending:second request:true");
});

test("hydration uses the server seed before a single live subscription supplies fresh data", async () => {
  const backend = fixture();
  const query = queryBinding<{ kind: "query"; $types: { input: undefined; output: string } }>(backend.scope, "title", false);
  const recoverable: unknown[] = [];
  function Title() {
    const state = useQuery(query, { initialData: "server" });
    return <p>{state.status}:{state.data}</p>;
  }
  container.innerHTML = renderToString(<Title />);
  expect(backend.calls).toBe(0);
  expect(backend.watches).toHaveLength(0);
  await act(async () => {
    root = client.hydrateRoot(container, <Title />, { onRecoverableError: error => { recoverable.push(error); } });
  });
  expect(container.textContent).toBe("pending:server");
  expect(recoverable).toEqual([]);
  expect(backend.watches).toHaveLength(1);
  await act(() => backend.watches[0]!.emit({ status: "success", data: "live", isStale: false, isOptimistic: false }));
  expect(container.textContent).toBe("success:live");
  expect(backend.calls).toBe(0);
});

test("Strict Mode disposes subscriptions and equivalent inline inputs preserve the current observation", async () => {
  const backend = fixture();
  const query = queryBinding<{ kind: "query"; $types: { input: { id: number; filter: { visible: boolean } }; output: string } }>(backend.scope, "title", true);
  function Title({ id, tick }: { id: number; tick: number }) {
    const state = useQuery(query, { id, filter: { visible: true } }, { initialData: "server for one" });
    return <p data-tick={tick}>{state.status}:{state.data}</p>;
  }
  await act(() => {
    root = client.createRoot(container);
    root.render(<StrictMode><Title id={1} tick={0} /></StrictMode>);
  });
  expect(backend.watches.filter(watch => watch.active)).toHaveLength(1);
  const first = backend.watches.find(watch => watch.active)!;
  await act(() => first.emit({ status: "success", data: "one", isStale: false, isOptimistic: false }));
  const starts = backend.watches.length;
  await act(() => root!.render(<StrictMode><Title id={1} tick={1} /></StrictMode>));
  expect(backend.watches).toHaveLength(starts);
  expect(container.textContent).toBe("success:one");
  await act(() => root!.render(<StrictMode><Title id={2} tick={2} /></StrictMode>));
  expect(first.active).toBe(false);
  expect(backend.watches.filter(watch => watch.active)).toHaveLength(1);
  expect(container.textContent).toBe("pending:");
  await act(() => first.emit({ status: "success", data: "obsolete", isStale: false, isOptimistic: false }));
  expect(container.textContent).toBe("pending:");
  const second = backend.watches.find(watch => watch.active)!;
  expect(second.operation.input).toEqual({ id: 2, filter: { visible: true } });
  await act(() => second.emit({ status: "success", data: "two", isStale: false, isOptimistic: false }));
  expect(container.textContent).toBe("success:two");
  await act(() => root!.render(<StrictMode><Title id={1} tick={3} /></StrictMode>));
  expect(container.textContent).toBe("pending:"); // Returning to the original input cannot revive its SSR seed.
  await act(() => root!.unmount());
  root = undefined;
  expect(backend.watches.filter(watch => watch.active)).toHaveLength(0);
  expect(backend.calls).toBe(0);
});

test("mutation hooks keep independent outcomes while rendering the last started write", async () => {
  const backend = fixture();
  const mutation = mutationBinding<{ kind: "mutation"; $types: { input: { title: string }; output: string } }>(backend.scope, "save", true);
  let save!: MutationResult<typeof mutation>;
  function Editor() {
    save = useMutation(mutation);
    return <p>{save.state.status}:{save.state.status === "success" ? save.state.data : ""}</p>;
  }
  expect(renderToString(<Editor />)).toContain("idle");
  expect(backend.calls).toBe(0);
  await act(() => {
    root = client.createRoot(container);
    root.render(<Editor />);
  });
  let first!: MutationHandle<string>;
  let second!: MutationHandle<string>;
  await act(() => {
    first = save({ title: "first" });
    second = save({ title: "second" });
  });
  expect(container.textContent).toBe("pending:");
  await act(() => backend.writes[1]!.recover());
  expect(container.textContent).toBe("recovering:");
  await act(async () => {
    backend.writes[1]!.resolve("second result");
    expect(await second).toBe("second result");
  });
  expect(container.textContent).toBe("success:second result");
  await act(async () => {
    backend.writes[0]!.resolve("first result");
    expect(await first).toBe("first result");
  });
  expect(container.textContent).toBe("success:second result");
  expect(backend.calls).toBe(2);
});

test("paginated hooks hydrate seeds, retain inline mapper observation and reset all pages on changed filters", async () => {
  const backend = fixture();
  type Definition = { kind: "query"; $types: { input: { filter: string; limit: number; cursor: string | null }; output: import("@hulla/db").Page<{ title: string }> } };
  const query = queryBinding<Definition>(backend.scope, "pages", true, undefined, true);
  let next!: (pages?: number) => Promise<void>;
  function List({ filter, tick = 0 }: { filter: string; tick?: number }) {
    const state = usePaginatedQuery(query, cursor => ({ filter, limit: 1, cursor }), { initialData: [{ title: "SSR" }] });
    next = state.next;
    return <p data-tick={tick}>{state.status}:{state.data?.map(row => row.title).join(",")}:{String(state.hasMore)}</p>;
  }
  container.innerHTML = renderToString(<List filter="one" />);
  expect(container.textContent).toBe("pending:SSR:false");
  expect(backend.watches).toHaveLength(0);
  expect(backend.calls).toBe(0);
  await act(async () => { root = client.hydrateRoot(container, <List filter="one" />); });
  expect(backend.watches).toHaveLength(1);
  const first = backend.watches[0]!;
  await act(async () => first.emit({ status: "success", data: { page: [{ title: "first" }], cursor: "after-first" }, isStale: false, isOptimistic: false }));
  expect(container.textContent).toBe("success:first:true");
  const stableNext = next;
  await act(() => root!.render(<List filter="one" tick={1} />));
  expect(backend.watches).toHaveLength(1);
  expect(next).toBe(stableNext);
  let appended!: Promise<void>;
  await act(async () => { appended = next(); });
  const second = backend.watches[1]!;
  expect(second.operation.input).toEqual({ filter: "one", limit: 1, cursor: "after-first" });
  await act(async () => {
    second.emit({ status: "success", data: { page: [{ title: "second" }], cursor: null }, isStale: false, isOptimistic: false });
    await appended;
  });
  expect(container.textContent).toBe("success:first,second:false");
  await act(async () => root!.render(<List filter="two" />));
  expect(first.active).toBe(false);
  expect(second.active).toBe(false);
  expect(container.textContent).toBe("pending::false");
  expect(backend.watches[2]!.operation.input).toEqual({ filter: "two", limit: 1, cursor: null });
  await act(() => first.emit({ status: "success", data: { page: [{ title: "late" }], cursor: null }, isStale: false, isOptimistic: false }));
  expect(container.textContent).toBe("pending::false");
  expect(next).toBe(stableNext);
  await act(async () => backend.watches[2]!.emit({ status: "success", data: { page: [{ title: "new first" }], cursor: "new-second" }, isStale: false, isOptimistic: false }));
  await act(async () => { appended = stableNext(); });
  expect(backend.watches[3]!.operation.input).toEqual({ filter: "two", limit: 1, cursor: "new-second" });
  await act(async () => {
    backend.watches[3]!.emit({ status: "success", data: { page: [{ title: "new second" }], cursor: null }, isStale: false, isOptimistic: false });
    await appended;
  });
  expect(container.textContent).toBe("success:new first,new second:false");
  await act(async () => root!.render(<List filter="one" />));
  expect(container.textContent).toBe("pending::false");
  await act(() => root!.unmount()); root = undefined;
  expect(backend.watches.every(watch => !watch.active)).toBe(true);
  expect(backend.calls).toBe(0);
});
