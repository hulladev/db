import { createTables } from "../src/fields.ts";
import { expect, test } from "bun:test";
import { transactionDatabase, type WriteDatabase } from "../src/database.ts";
import { defineSchema, defineTable, parseRow, v } from "../src/schema.ts";

const schema = defineSchema({ tasks: defineTable({ title: v.string(), done: v.boolean().default(false), rank: v.int64({ mode: "bigint" }).nullable() }) });
const row = { id: "019a0000-0000-7000-8000-000000000001", title: "Draft", done: false, rank: "12" };
const decoded = parseRow(schema, "tasks", { ...row, rank: 12n });
const stored = { row, version: "1" };
function database(request: (op: Record<string, unknown>) => Promise<unknown>) {
  return transactionDatabase(schema, true, request) as WriteDatabase<typeof schema>;
}

test("database builders execute lazily once, preserve branches, and reject changes after consumption", async () => {
  const requests: Record<string, unknown>[] = [];
  const db = database(async op => { requests.push(op); return [stored]; });
  const base = db.select("tasks");
  const draft = base.where("title", "=", "Draft");
  const done = base.where({ done: true }).limit(1);
  expect(requests).toHaveLength(0);
  const [first, same] = await Promise.all([draft, draft]);
  expect(first).toBe(same);
  expect(first).toEqual([decoded]);
  expect(requests).toHaveLength(1);
  expect(() => draft.where({ done: false })).toThrow("after execution");
  await done;
  await base;
  expect(requests.map(op => op.filters)).toEqual([
    [{ kind:"compare", field: "title", operator:"=", operand:{kind:"value",value: "Draft"} }],
    [{ kind:"compare", field: "done", operator:"=", operand:{kind:"value",value: true} }],
    [],
  ]);
  expect(requests.map(op => op.limit)).toEqual([null, 1, null]);
});

test("failed execution is also memoized and unique checks cardinality instead of hiding duplicates", async () => {
  let calls = 0;
  const failure = new Error("storage unavailable");
  const db = database(async () => { calls++; throw failure; });
  const query = db.select("tasks");
  await expect(Promise.resolve(query)).rejects.toBe(failure);
  await expect(Promise.resolve(query)).rejects.toBe(failure);
  expect(calls).toBe(1);
  const results: unknown[][] = [[], [stored], [stored, stored]];
  const unique = database(async () => results.shift()!);
  expect(await unique.select("tasks").unique()).toBeNull();
  expect(await unique.select("tasks").unique()).toEqual(decoded);
  await expect(Promise.resolve(unique.select("tasks").unique())).rejects.toMatchObject({ code: "not_unique" });
});

test("unscoped writes cannot execute and repeated filters remain conjunctive even for the same field", async () => {
  const requests: Record<string, unknown>[] = [];
  const db = database(async op => { requests.push(op); return []; });
  await expect(Promise.resolve(db.delete("tasks"))).rejects.toThrow("require where");
  await expect(Promise.resolve(db.update("tasks", { title: "New" }))).rejects.toThrow("require where");
  expect(() => db.delete("tasks").where({})).toThrow("where(true)");
  expect(requests).toHaveLength(0);
  expect(await db.update("tasks", { title: "New" }).where("title", "=", "Draft").where({ title: "Published" })).toEqual([]);
  expect(requests[0]?.filters).toEqual([{ kind:"compare", field: "title", operator:"=", operand:{kind:"value",value: "Draft"} }, { kind:"compare", field: "title", operator:"=", operand:{kind:"value",value: "Published"} }]);
  await db.delete("tasks").where(true);
  expect(requests[1]?.filters).toEqual([{kind:"constant",value:true}]);
});

test("insert builders capture validated inputs, return plain rows, and batch without implicit truncation", async () => {
  const requests: Record<string, unknown>[] = [];
  const db = database(async op => { requests.push(op); return Array.isArray(op.values) ? op.values.map(() => stored) : stored; });
  const input = { title: "Draft", rank: 12n };
  const {tasks} = createTables<typeof schema>({tasks:{"id":{},"title":{},"done":{},"rank":{}}});
  const one = db.insert(tasks, input);
  input.title = "Changed later";
  expect(requests).toHaveLength(0);
  expect(await one).toEqual(decoded);
  expect(requests[0]?.values).toEqual({ title: "Draft", rank: "12" });
  const batch = Array.from({ length: 1001 }, () => ({ title: "Bulk", rank: null }));
  expect(await db.insert("tasks", batch)).toHaveLength(1001);
  expect(requests).toHaveLength(2);
  expect(await db.insert("tasks", [])).toEqual([]);
  expect(requests).toHaveLength(2);
});

test("invalid filters and limits fail before transport, with explicit null preserved", async () => {
  const requests: Record<string, unknown>[] = [];
  const db = database(async op => { requests.push(op); return []; });
  expect(() => db.select("tasks").where({ title: undefined } as never)).toThrow();
  expect(() => db.select("tasks").where("title", "=" as never, 3 as never)).toThrow();
  expect(() => db.select("tasks").where({ missing: true } as never)).toThrow();
  for (const count of [0, -1, NaN, Infinity, 1.5]) expect(() => db.select("tasks").limit(count)).toThrow();
  expect(requests).toHaveLength(0);
  await db.select("tasks").where({ rank: null });
  expect(requests[0]?.filters).toEqual([{ kind:"compare", field: "rank", operator:"is", operand:{kind:"value",value: null} }]);
});


test("patches ignore undefined known fields but reject undeclared fields and explicit invalid null", async () => {
  let written: unknown;
  const db = database(async op => { written = op.values; return [stored]; });
  await db.update("tasks", { title: undefined, rank: null }).where(true);
  expect(written).toEqual({ rank: { kind: "value", value: null } });
  expect(() => db.update("tasks", { missing: undefined } as never)).toThrow("Unknown field");
  expect(() => db.update("tasks", { title: null } as never)).toThrow();
});

// Protect grouping and ordering across immutable branches and terminal execution.
test("nested predicates preserve complete groups, identities, fields and ordering", async () => {
  const requests:Record<string,unknown>[]=[];
  const db=database(async op=>{requests.push(op);return [];});
  const {tasks}=createTables<typeof schema>({tasks:{"id":{},"title":{},"done":{},"rank":{}}});
  const base=db.select(tasks).orderBy(tasks.rank,"desc");
  await base.where(({and,or,not,eq,contains})=>and(or(eq(tasks.title,"Draft"),contains("title","a%_",{caseSensitive:false})),not(or()))).where(({eq})=>eq(tasks.rank,tasks.rank)).orderBy("title").limit(3);
  await base.unique();
  expect(requests[0]?.order).toEqual([{field:"rank",direction:"desc"},{field:"title",direction:"asc"}]);
  expect(requests[1]?.order).toEqual([{field:"rank",direction:"desc"}]);
  expect(requests[0]?.filters).toEqual([
    {kind:"and",predicates:[{kind:"or",predicates:[{kind:"compare",field:"title",operator:"=",operand:{kind:"value",value:"Draft"}},{kind:"compare",field:"title",operator:"contains",operand:{kind:"value",value:"a%_"},caseSensitive:false}]},{kind:"not",predicate:{kind:"constant",value:false}}]},
    {kind:"compare",field:"rank",operator:"=",operand:{kind:"field",field:"rank"}},
  ]);
  await db.delete(tasks).where(({and})=>and());
  expect(requests[2]?.filters).toEqual([{kind:"constant",value:true}]);
  expect(()=>db.select(tasks).where(({or})=>or(undefined as never))).toThrow();
  expect(()=>db.select(tasks).where("rank","in",[undefined] as never)).toThrow();
  expect(()=>db.select(tasks).where("rank","between",[null,1n] as never)).toThrow();
  expect(()=>db.select(tasks).where("done",">" as never,true as never)).toThrow();
  expect(()=>db.select(tasks).where("title","=",tasks.rank as never)).toThrow();
  const foreign=createTables<typeof schema>({other:{"title":{}}}) as unknown as {other:{title:typeof tasks.title}};
  expect(()=>db.select(tasks).where(foreign.other.title,"=","Draft")).toThrow();
});

test("callback predicates cannot escape table scope and capture input arrays by value",async()=>{
  const secondSchema=defineSchema({tasks:defineTable({title:v.string()}),other:defineTable({title:v.string()})});
  const requests:Record<string,unknown>[]=[];
  const db=transactionDatabase(secondSchema,true,async op=>{requests.push(op);return [];}) as WriteDatabase<typeof secondSchema>;
  let captured:import("../src/filters.ts").Predicate;
  db.select("other").where(({eq})=>captured=eq("title","other"));
  expect(()=>db.select("tasks").where(()=>captured)).toThrow("selected table");
  const values=["original"];
  const query=db.select("tasks").where(({inArray})=>inArray("title",values));
  values[0]="mutated";
  await query;
  expect(requests[0]?.filters).toEqual([{kind:"compare",field:"title",operator:"in",operand:{kind:"value",value:["original"]}}]);
  expect(()=>db.select("tasks").where("title","contains","x",{caseSensitive:"yes"} as never)).toThrow();
  await db.select("tasks").where(({inArray})=>inArray("title",[null,"match"]));
  expect(requests[1]?.filters).toEqual([{kind:"compare",field:"title",operator:"in",operand:{kind:"value",value:[null,"match"]}}]);
  db.select("tasks").where(({and,eq})=>{
    const group=and(eq("title","original"));
    if(group.kind!=="and") throw new Error("Expected group");
    expect(()=>group.predicates.push(group)).toThrow();
    return group;
  });
});

test("pagination preserves selection and decodes rows without exposing range metadata", async () => {
  const requests: Record<string, unknown>[] = [];
  const db = database(async op => { requests.push(op); return { page: [stored], cursor: "next", range: "private-range" }; });
  const pending = db.select("tasks").where({ done: false }).orderBy("title", "desc").paginate({ limit: 20 });
  expect(requests).toHaveLength(0);
  const result = await pending;
  expect(await pending).toBe(result);
  expect(result as unknown).toEqual({ page: [decoded], cursor: "next" });
  expect(requests).toEqual([{ method: "paginate", table: "tasks", filters: [{ kind: "compare", field: "done", operator: "=", operand: { kind: "value", value: false } }], order: [{ field: "title", direction: "desc" }], limit: 20, cursor: null }]);
  for (const limit of [0, -1, 1.5, 1001, Number.MAX_SAFE_INTEGER + 1, NaN]) {
    expect(() => db.select("tasks").paginate({ limit })).toThrow("Page limit");
  }
  expect(() => db.select("tasks").paginate({ limit: 1, cursor: 4 as never })).toThrow("Page cursor");
  expect(requests).toHaveLength(1);
});

test("projections capture inclusion choices and decode only selected fields while retaining full-row filters", async () => {
  const requests: Record<string, unknown>[] = [];
  const db = database(async op => {
    requests.push(op);
    return op.method === "paginate"
      ? { page: [{ row: { rank: "12" }, version: "1" }], cursor: null, range: "private" }
      : [{ row: { title: "Draft", rank: null }, version: "1" }];
  });
  const fields: ("title" | "rank" | false | null | undefined)[] = ["title", false, null, "rank", undefined, "title"];
  const query = db.select("tasks", fields).where("done", "=", false).orderBy("id");
  fields.splice(0, fields.length, "rank");
  expect(requests).toHaveLength(0);
  const result = await query;
  expect(result).toEqual([{ title: "Draft", rank: null }]);
  expect(await query).toBe(result);
  expect(requests[0]?.projection).toEqual(["title", "rank"]);
  expect(requests[0]?.filters).toMatchObject([{ field: "done" }]);
  expect(requests[0]?.order).toEqual([{ field: "id", direction: "asc" }]);
  const include = { title: false, rank: true, done: null, id: undefined };
  const page = db.select("tasks", include).paginate({ limit: 1 });
  include.rank = false;
  expect(await page as unknown).toEqual({ page: [{ rank: 12n }], cursor: null });
  expect(requests[1]?.projection).toEqual(["rank"]);
});

test("returning preserves immutable write branches, bulk shape, scoping, and single execution", async () => {
  const requests: Record<string, unknown>[] = [];
  const responses: unknown[] = [
    { row: { title: "Draft" }, version: "1" },
    [{ row: { rank: "12" }, version: "1" }],
    [{ row: { title: "Draft" }, version: "1" }],
    [{ row: { done: false }, version: "1" }],
  ];
  const db = database(async op => { requests.push(op); return responses.shift(); });
  const base = db.insert("tasks", { title: "Draft", rank: 12n });
  const selected = base.returning(["title"]);
  const [first, again] = await Promise.all([selected, selected]);
  expect(first).toEqual({ title: "Draft" });
  expect(first).toBe(again);
  expect(() => selected.returning(["rank"])).toThrow("after execution");
  expect(await db.insert("tasks", [{ title: "Draft", rank: 12n }]).returning({ rank: true })).toEqual([{ rank: 12n }]);
  const pending = db.update("tasks", { title: "New" }).returning(["title"]);
  await expect(Promise.resolve(pending)).rejects.toThrow("require where");
  const scoped = db.update("tasks", { title: "New" }).returning(["title"]).where({ done: false });
  expect(await scoped).toEqual([{ title: "Draft" }]);
  expect(() => scoped.returning(["rank"])).toThrow("after execution");
  expect(await db.delete("tasks").where({ title: "Draft" }).returning(["done"])).toEqual([{ done: false }]);
  expect(requests.map(op => op.projection)).toEqual([["title"], ["rank"], ["title"], ["done"]]);
  expect(requests[2]?.values).toEqual({ title: { kind: "value", value: "New" } });
  expect(await db.insert("tasks", []).returning(["title"])).toEqual([]);
  expect(requests).toHaveLength(4);
});

test("invalid projections and malformed projected rows fail instead of leaking or defaulting fields", async () => {
  let calls = 0;
  const db = database(async () => { calls++; return []; });
  for (const input of [[], {}, [false, null, undefined], { title: false }, ["missing"], { missing: false }, [""], [0], { title: 1 }, { title: "title" }]) {
    expect(() => db.select("tasks", input as never)).toThrow();
    expect(() => db.insert("tasks", []).returning(input as never)).toThrow();
    expect(() => db.update("tasks", { done: true }).returning(input as never)).toThrow();
  }
  expect(calls).toBe(0);
  for (const invalid of [{}, { rank: "12", title: "sensitive" }, { rank: "invalid" }, { rank: 12 }]) {
    const malformed = database(async () => [{ row: invalid, version: "1" }]);
    await expect(Promise.resolve(malformed.select("tasks", ["rank"]))).rejects.toThrow();
  }
});

test("JSON filters preserve paths, null membership, and array length while rejecting invalid operands", async () => {
  const jsonSchema = defineSchema({ items: defineTable({ settings: v.json({ theme: v.string().optional(), tags: v.array(v.string().nullable()).nullable(), $value: v.string() }) }) });
  const { items } = createTables<typeof jsonSchema>({ items: { id: {}, settings: { jsonShape: { kind: "object", fields: {
    theme: { shape: { kind: "string" }, optional: true },
    tags: { shape: { kind: "nullable", inner: { kind: "array", item: { kind: "nullable", inner: { kind: "string" } } } }, optional: false },
    $value: { shape: { kind: "string" }, optional: false },
  } } } } });
  const requests: Record<string, unknown>[] = [];
  const db = transactionDatabase(jsonSchema, true, async op => { requests.push(op); return []; }) as WriteDatabase<typeof jsonSchema>;
  await db.select(items).where("settings.theme", "exists").where("settings.theme", "=", null)
    .where(items.settings.tags, "contains", null)
    .where(({ containsAll, not }) => not(containsAll(items.settings.tags, [])))
    .where(({ gte, arrayLength }) => gte(arrayLength(items.settings.tags), 3))
    .where(items.settings.$value, "contains", "%_")
    .where(({ eq, arrayLength }) => eq(arrayLength(items.settings.tags), arrayLength(items.settings.tags)));
  const filters = requests[0]!.filters as Array<Record<string, unknown>>;
  expect(filters[0]).toMatchObject({ field: "settings.theme", operator: "exists", operand: { value: true } });
  expect(filters[1]).toMatchObject({ field: "settings.theme", operator: "is" });
  expect(filters[2]).toMatchObject({ field: ["settings", "tags"], operator: "contains", operand: { value: null } });
  expect(filters[3]).toMatchObject({ kind: "not", predicate: { operator: "containsAll", operand: { value: [] } } });
  expect(filters[4]).toMatchObject({ field: ["settings", "tags"], transform: "arrayLength", operator: ">=", operand: { value: 3 } });
  expect(filters[6]).toMatchObject({ transform: "arrayLength", operand: { kind: "field", field: ["settings", "tags"], transform: "arrayLength" } });
  expect(filters[5]).toMatchObject({ field: ["settings", "$value"], operator: "contains", operand: { value: "%_" } });
  expect(() => db.select(items).where("settings.tags", "contains", 1 as never)).toThrow();
  expect(() => db.select(items).where("settings.tags", "containsAny", [undefined] as never)).toThrow();
  expect(() => db.select(items).where("settings.theme", "contains", "x", { caseSensitive: false })).not.toThrow();
  expect(() => db.select(items).orderBy("settings")).toThrow("JSON ordering");
});

test("generated references disambiguate literal dotted JSON keys from path strings", async () => {
  const shape = { kind: "object", fields: { "a.b": { shape: { kind: "string" }, optional: false }, a: { shape: { kind: "object", fields: { b: { shape: { kind: "string" }, optional: false } } }, optional: false } } } as const;
  const dottedSchema = defineSchema({ items: defineTable({ "settings.theme": v.string(), "settings.a.b": v.string(), settings: v.json({ "a.b": v.string(), a: v.object({ b: v.string() }) }) }) });
  const { items } = createTables<typeof dottedSchema>({ items: { id: {}, "settings.theme": {}, "settings.a.b": {}, settings: { jsonShape: shape } } });
  const requests: Record<string, unknown>[] = [];
  const db = transactionDatabase(dottedSchema, false, async op => { requests.push(op); return []; });
  await db.select(items).where(items.settings["a.b"], "=", "literal").where(items.settings.a.b, "=", "nested").where("settings.theme", "=", "column");
  expect(() => db.select(items).orderBy(items.settings.a.b)).toThrow("JSON path ordering");
  expect(() => db.select(items).orderBy("settings.a.b")).not.toThrow();
  expect(() => db.select(items).orderBy(items["settings.a.b"])).not.toThrow();
  expect(requests[0]!.filters).toMatchObject([
    { field: ["settings", "a.b"], operand: { value: "literal" } },
    { field: ["settings", "a", "b"], operand: { value: "nested" } },
    { field: "settings.theme", operand: { value: "column" } },
  ]);
});
