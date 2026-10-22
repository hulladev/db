import { expect, test } from "bun:test";
import { OptimisticResults, type Predictor } from "../src/optimistic.ts";
import { valueCodec } from "../src/codec.ts";

const source = "source", query = { name: "items.all" };
type Item = { id: string; title: string };
const input = valueCodec({ kind: "undefined" });
const output = valueCodec({ kind: "array", item: { kind: "object", fields: {
  id: { shape: { kind: "string" }, optional: false }, title: { shape: { kind: "string" }, optional: false },
} } });
const operation = { name: query.name, kind: "query" as const, input: undefined, codecs: { input, result: output } };
function fixture(rows: Item[] = []) {
  const layers = new OptimisticResults(target => {
    if (!["items.all", "items.other"].includes(target.name) || target.input !== undefined) throw new TypeError("Invalid query");
    return { key: target.name, operation: { ...operation, name: target.name } };
  });
  layers.confirm(query.name, operation, rows, source, "0");
  return layers;
}
const append: Predictor = (input, context) => {
  const rows = context.get(query) as Item[];
  context.set(query, [...rows, { id: context.requestId, title: String(input) }]);
};

test("rejection rebases surviving layers over remote changes without restoring old confirmed data", () => {
  const layers = fixture([{ id: "base", title: "old" }]);
  layers.begin("a", "items.create", "A", append);
  layers.begin("b", "items.create", "B", append);
  layers.confirm(query.name, operation, [{ id: "base", title: "remote edit" }, { id: "remote", title: "added remotely" }], source, "1");
  layers.reject("a");
  expect(layers.view(query.name)?.data).toEqual([
    { id: "base", title: "remote edit" }, { id: "remote", title: "added remotely" }, { id: "b", title: "B" },
  ]);
  const leaked = layers.view(query.name)?.data as Item[]; leaked[0]!.title = "observer mutation";
  expect((layers.view(query.name)?.data as Item[])[0]!.title).toBe("remote edit");
});

test("committed predictions retire per exact result visibility fence and canonical values replace speculation", () => {
  const layers = fixture();
  const other = { ...operation, name: "items.other" };
  layers.confirm(other.name, other, [], source, "0");
  layers.begin("request", "create", "predicted", (_, context) => {
    append("predicted", context);
    context.set({ name: other.name }, [{ id: "request", title: "predicted" }]);
  });
  layers.commit("request", source, "2");
  expect((layers.view(query.name)?.data as Item[])[0]!.title).toBe("predicted");
  layers.confirm(query.name, operation, [{ id: "server-id", title: "Canonical" }], source, "2");
  expect(layers.view(query.name)).toEqual({ data: [{ id: "server-id", title: "Canonical" }], isOptimistic: false });
  expect(layers.view(other.name)?.isOptimistic).toBe(true);
  layers.confirm(other.name, other, [], source, "3"); // Filtered/top-N result may exclude the committed row.
  expect(layers.view(other.name)).toEqual({ data: [], isOptimistic: false });
  expect(layers.unconfirmed()).toEqual([]);
});

test("uncached targets are skipped permanently and malformed predictions cannot publish partially", () => {
  const layers = fixture();
  layers.begin("missing", "create", "A", (_, context) => context.set({ name: "items.other" }, []));
  const other = { ...operation, name: "items.other" };
  layers.confirm(other.name, other, [{ id: "remote", title: "later" }], source, "1");
  expect(layers.view(other.name)).toEqual({ data: [{ id: "remote", title: "later" }], isOptimistic: false });
  layers.begin("bad", "create", "Bad", (_, context) => {
    context.set(query, [{ id: "prediction", title: "must not publish" }]);
    context.set({ name: other.name }, [{ id: 1, title: "invalid" }]);
  });
  expect(layers.view(query.name)).toEqual({ data: [], isOptimistic: false });
  expect(layers.view(other.name)).toEqual({ data: [{ id: "remote", title: "later" }], isOptimistic: false });
  expect(layers.unconfirmed()).toEqual([]);
});

test("an incompatible remote change removes only the failed invocation and replays surviving layers", () => {
  const layers = fixture([{ id: "base", title: "Original" }]);
  layers.begin("edit", "edit", "Updated", (input, context) => {
    const rows = context.get(query) as Item[];
    if (!rows.some(row => row.id === "base")) throw new Error("missing prediction input");
    context.set(query, rows.map(row => row.id === "base" ? { ...row, title: String(input) } : row));
  });
  layers.begin("other", "create", "Independent", append);
  layers.confirm(query.name, operation, [], source, "1");
  expect(layers.view(query.name)?.data).toEqual([{ id: "other", title: "Independent" }]);
  expect(layers.unconfirmed().map(layer => layer.requestId)).toEqual(["other"]);
  layers.removeResult(query.name);
  expect(layers.view(query.name)).toBeUndefined();
  layers.clear(); expect(layers.unconfirmed()).toEqual([]);
});

test("query matching replays only originally cached targets and cannot acquire later variants", () => {
  const layers = fixture();
  layers.begin("all", "create", "Added", (_, context) => {
    for (const target of [...context.queries("items.all"), ...context.queries("items.other")]) {
      context.set(target, [...context.get(target) as Item[], { id: "predicted", title: "Added" }]);
    }
  });
  const other = { ...operation, name: "items.other" };
  layers.confirm(other.name, other, [], source, "1");
  expect(layers.view(query.name)?.data).toEqual([{ id: "predicted", title: "Added" }]);
  expect(layers.view(other.name)).toEqual({ data: [], isOptimistic: false });
});

test("coherent round rebases a cross-query prediction against all new bases together", () => {
  const layers = fixture(), other = { ...operation, name: "items.other" };
  layers.confirm(other.name, other, [], source, "0");
  layers.begin("pending", "create", undefined, (_, context) => {
    const first = context.get(query) as Item[], second = context.get({ name: other.name }) as Item[];
    if (first.length !== second.length) throw new Error("related result sizes disagree");
    context.set(query, [...first, { id: "pending", title: "prediction" }]);
    context.set({ name: other.name }, [...second, { id: "pending", title: "prediction" }]);
  });
  const rows = [{ id: "remote", title: "remote" }];
  layers.confirmRound([
    { key: query.name, operation, data: rows, source, revision: "1" },
    { key: other.name, operation: other, data: rows, source, revision: "1" },
  ]);
  expect(layers.view(query.name)?.isOptimistic).toBe(true);
  expect(layers.view(other.name)?.data).toEqual([...rows, { id: "pending", title: "prediction" }]);
});

test.each([false, true])("generated optimistic selectors update same-input owned pages independently (match=%s)", async (match) => {
  const { createRuntimeScope } = await import("../src/runtime.ts");
  const { mutationBinding, queryBinding } = await import("../src/callable.ts");
  const { operationKey } = await import("../src/http/protocol.ts");
  type Page = import("../src/page.ts").Page<Item>;
  type QueryDefinition = { kind: "query"; $types: { input: undefined; output: Page } };
  type MutationDefinition = { kind: "mutation"; $types: { input: string; output: string } };
  const pagedOutput = valueCodec({ kind: "object", fields: {
    page: { shape: { kind: "array", item: { kind: "object", fields: {
      id: { shape: { kind: "string" }, optional: false }, title: { shape: { kind: "string" }, optional: false },
    } } }, optional: false },
    cursor: { shape: { kind: "union", choices: [{ kind: "string" }, { kind: "null" }] }, optional: false },
  } });
  const codecs = { input, result: pagedOutput };
  const owned = ["first-owner", "second-owner"].map(owner => ({
    name: "items.paged", kind: "query" as const, input: undefined, codecs, page: { owner, range: `range-${owner}` },
  }));
  const layers = new OptimisticResults(target => {
    const operation = { name: target.name, kind: "query" as const, input: target.input, codecs };
    return { key: operationKey(operation), operation };
  });
  const confirm = (revision: string, title: string) => layers.confirmRound(owned.map((operation, index) => ({
    key: operationKey(operation), operation, data: { page: [{ id: String(index), title }], cursor: "confirmed-cursor" }, source, revision,
  })));
  confirm("0", "original");
  const scope = createRuntimeScope();
  let request = 0;
  let settle!: (commit: boolean) => void;
  const detach = scope.configure(() => ({
    async call(operation, options) {
      const id = String(++request);
      layers.begin(id, operation.name, operation.input, options.optimistic!);
      return await new Promise<string>((resolve, reject) => {
        settle = commit => {
          if (commit) {
            layers.commit(id, source, "1");
            confirm("1", "server-canonical");
            resolve("saved");
          } else {
            layers.reject(id);
            reject(new Error("rejected"));
          }
        };
      });
    },
  }));
  try {
    const query = queryBinding<QueryDefinition>(scope, "items.paged", false, codecs, true);
    const mutation = mutationBinding<MutationDefinition>(scope, "items.edit", true).withOptimistic({
      query,
      ...(match ? { match: () => true } : {}),
      update: (current, title) => ({ ...current, page: current.page.map(row => ({ ...row, title })), cursor: null }),
    });
    const rejected = mutation("predicted");
    for (const operation of owned) {
      expect(layers.view(operationKey(operation))).toMatchObject({
        data: { page: [{ title: "predicted" }], cursor: "confirmed-cursor" }, isOptimistic: true,
      });
      expect(operation.page.range).toBe(`range-${operation.page.owner}`);
    }
    settle(false);
    await expect(Promise.resolve(rejected)).rejects.toThrow("rejected");
    for (const operation of owned) expect(layers.view(operationKey(operation))).toMatchObject({ data: { page: [{ title: "original" }] } });
    const committed = mutation("second prediction");
    settle(true);
    expect(await committed).toBe("saved");
    for (const operation of owned) {
      expect(layers.view(operationKey(operation))).toMatchObject({ data: { page: [{ title: "server-canonical" }], cursor: "confirmed-cursor" } });
      expect(layers.view(operationKey(operation))?.isOptimistic).toBe(false);
    }
  } finally { detach(); }
});
