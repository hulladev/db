import { expect, test } from "bun:test";
import { createRenderer, createSSRApp, defineComponent, h, nextTick, ref } from "vue";
import { renderToString } from "@vue/server-renderer";
import { createRuntimeScope } from "../../db/src/runtime.ts";
import { mutationBinding, queryBinding } from "../../db/src/callable.ts";
import { type FunctionRuntime, type Operation } from "../../db/src/contracts.ts";
import { type Page, type QueryState } from "@hulla/db";
import { useMutation, usePaginatedQuery, useQuery } from "../src/index.ts";

type HostNode = { children: HostNode[]; text: string };
const node = (): HostNode => ({ children: [], text: "" });
const renderer = createRenderer<HostNode, HostNode>({
  patchProp() {}, createElement: node, createText: text => ({ children: [], text }), createComment: node,
  insert: (child, parent) => { parent.children.push(child); }, remove() {},
  setText: (target, text) => { target.text = text; }, setElementText: (target, text) => { target.text = text; },
  parentNode: () => null, nextSibling: () => null,
});

function fixture() {
  const scope = createRuntimeScope();
  const watches: { operation: Operation; emit: (state: QueryState<unknown>) => void; active: boolean }[] = [];
  let resolveWrite!: (value: unknown) => void;
  let calls = 0;
  const runtime: FunctionRuntime = {
    call(operation) {
      calls++;
      if (operation.kind === "query") throw new Error("Bindings must observe instead of issuing reads");
      return new Promise(resolve => { resolveWrite = resolve; });
    },
    watch(operation, emit) {
      const watch = { operation, emit, active: true }; watches.push(watch);
      emit({ status: "pending" });
      return () => { watch.active = false; };
    },
  };
  const dispose = scope.configure(() => runtime);
  return { scope, watches, dispose, resolve: (value: unknown) => resolveWrite(value), get calls() { return calls; } };
}

test("SSR isolates initialData without starting observation", async () => {
  const backend = fixture();
  try {
    const query = queryBinding<{ kind: "query"; $types: { input: undefined; output: string } }>(backend.scope, "title", false);
    const App = defineComponent({ props: ["seed"], setup(props) {
      const state = useQuery(query, { initialData: props.seed as string });
      return () => h("p", `${state.value.status}:${state.value.data}`);
    } });
    expect(await renderToString(createSSRApp(App, { seed: "first" }))).toBe("<p>pending:first</p>");
    expect(await renderToString(createSSRApp(App, { seed: "second" }))).toBe("<p>pending:second</p>");
    expect(backend.watches).toHaveLength(0);
    expect(backend.calls).toBe(0);
  } finally { backend.dispose(); }
});

test("mounted queries follow input changes, discard seeds, fence late results and unsubscribe", async () => {
  const backend = fixture();
  const id = ref(1);
  const query = queryBinding<{ kind: "query"; $types: { input: { id: number }; output: string } }>(backend.scope, "title", true);
  let state!: ReturnType<typeof useQuery<typeof query>>;
  const app = renderer.createApp(defineComponent({ setup() {
    state = useQuery(query, () => ({ id: id.value }), { initialData: "server" });
    expect(backend.watches).toHaveLength(0);
    return () => h("p", state.value.data);
  } }));
  try {
    app.mount(node());
    expect(state.value.data).toBe("server");
    backend.watches[0]!.emit({ status: "success", data: "one", isStale: false, isOptimistic: false });
    await nextTick();
    expect(state.value.data).toBe("one");
    id.value = 2; await nextTick();
    expect(backend.watches[0]!.active).toBe(false);
    expect(backend.watches[1]!.operation.input).toEqual({ id: 2 });
    expect(state.value.data).toBeUndefined();
    backend.watches[0]!.emit({ status: "success", data: "late", isStale: false, isOptimistic: false });
    expect(state.value.data).toBeUndefined();
    id.value = 1; await nextTick();
    expect(state.value.data).toBeUndefined();
    expect(backend.calls).toBe(0);
  } finally { app.unmount(); backend.dispose(); }
  expect(backend.watches.every(watch => !watch.active)).toBe(true);
});

test("mutation state follows the mounted callable and resolves its handle", async () => {
  const backend = fixture();
  const mutation = mutationBinding<{ kind: "mutation"; $types: { input: { title: string }; output: string } }>(backend.scope, "save", true);
  let save!: ReturnType<typeof useMutation<typeof mutation>>;
  const app = renderer.createApp(defineComponent({ setup() { save = useMutation(mutation); return () => h("p", save.state.value.status); } }));
  try {
    app.mount(node());
    expect(save.state.value.status).toBe("idle");
    const handle = save({ title: "new" });
    expect(save.state.value.status).toBe("pending");
    backend.resolve("saved");
    expect(await handle).toBe("saved"); await nextTick();
    expect(save.state.value).toEqual({ status: "success", data: "saved" });
  } finally { app.unmount(); backend.dispose(); }
});


test("paginated refs append live pages, reset reactive filters and keep next bound to the current list", async () => {
  const backend = fixture();
  const filter = ref("open");
  const tick = ref(0);
  const query = queryBinding<{ kind: "query"; $types: { input: { filter: string; cursor: string | null }; output: Page<{ id: number }> } }>(backend.scope, "tasks", true, undefined, true);
  let state!: ReturnType<typeof usePaginatedQuery<typeof query>>;
  const app = renderer.createApp(defineComponent({ setup() {
    state = usePaginatedQuery(query, cursor => { tick.value; return { filter: filter.value, cursor }; }, { initialData: [{ id: 0 }] });
    expect(backend.watches).toHaveLength(0);
    return () => h("p", state.value.data?.map(row => row.id).join(","));
  } }));
  const emit = (index: number, ids: number[], cursor: string | null) => backend.watches[index]!.emit({ status: "success", data: { page: ids.map(id => ({ id })), cursor }, isStale: false, isOptimistic: false });
  let next!: typeof state.next;
  try {
    app.mount(node()); next = state.next; await Bun.sleep(0);
    expect(state.value.data).toEqual([{ id: 0 }]);
    emit(0, [1], "second"); await Bun.sleep(0);
    expect(state.value.hasMore).toBe(true);
    tick.value++; await nextTick();
    expect(backend.watches).toHaveLength(1);
    const append = next(); await Bun.sleep(0);
    expect(backend.watches[1]!.operation.input).toEqual({ filter: "open", cursor: "second" });
    expect(state.value.status).toBe("pending");
    expect(state.value.data).toEqual([{ id: 1 }]);
    emit(1, [2], null); await append;
    expect(state.value.data).toEqual([{ id: 1 }, { id: 2 }]);
    expect(state.value.hasMore).toBe(false);
    filter.value = "closed"; await nextTick(); await Bun.sleep(0);
    expect(backend.watches.slice(0, 2).every(watch => !watch.active)).toBe(true);
    expect(state.value.data).toBeUndefined();
    expect(backend.watches[2]!.operation.input).toEqual({ filter: "closed", cursor: null });
    emit(0, [99], null);
    expect(state.value.data).toBeUndefined();
    emit(2, [3], "closed-second"); await Bun.sleep(0);
    const nextFilterPage = next(); await Bun.sleep(0);
    expect(backend.watches[3]!.operation.input).toEqual({ filter: "closed", cursor: "closed-second" });
    emit(3, [4], null); await nextFilterPage;
    expect(state.value.data).toEqual([{ id: 3 }, { id: 4 }]);
    filter.value = "open"; await nextTick(); await Bun.sleep(0);
    expect(state.value.data).toBeUndefined();
    expect(backend.calls).toBe(0);
  } finally { app.unmount(); backend.dispose(); }
  expect(backend.watches.every(watch => !watch.active)).toBe(true);
});

test("paginated SSR keeps flattened seeds request-local without watching pages", async () => {
  const backend = fixture();
  const query = queryBinding<{ kind: "query"; $types: { input: { cursor: string | null }; output: Page<{ id: number }> } }>(backend.scope, "tasks", true, undefined, true);
  const App = defineComponent({ props: ["seed"], setup(props) {
    const state = usePaginatedQuery(query, cursor => ({ cursor }), { initialData: [{ id: props.seed as number }] });
    return () => h("p", `${state.value.status}:${state.value.data?.[0]?.id}:${state.value.hasMore}`);
  } });
  try {
    expect(await renderToString(createSSRApp(App, { seed: 1 }))).toBe("<p>pending:1:false</p>");
    expect(await renderToString(createSSRApp(App, { seed: 2 }))).toBe("<p>pending:2:false</p>");
    expect(backend.watches).toHaveLength(0);
    expect(backend.calls).toBe(0);
  } finally { backend.dispose(); }
});
