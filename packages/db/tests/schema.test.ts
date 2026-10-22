import { access, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { expect, test } from "bun:test";
import {
  compileSchema,
  defineSchema,
  defineTable,
  parsePatch,
  parseRow,
  prepareInsert,
  v,
  type TableOptions,
} from "../src/schema.ts";
import {
  generateFunctions,
  generateSchema,
} from "../src/codegen.ts";
import schema from "./fixtures/schema.ts";

const publication = "daily";
const draft = { publication, title: "First", excerpt: null };
const articleId = "AAAAAAAA-0000-0000-0000-000000000002";
const source = new URL("./fixtures/schema.ts", import.meta.url).pathname;

test("user-defined rows, inserts and patches agree on defaults, omissions and immutable keys", () => {
  const prepared = prepareInsert(schema, "articles", draft);
  expect(prepared).toMatchObject({
    ...draft,
    published: false,
    ordinal: 9223372036854775807n,
    origin: "aaaaaaaa-0000-0000-0000-000000000001",
  });
  expect(Object.hasOwn(prepared, "id")).toBe(false);
  expect(draft).toEqual({ publication, title: "First", excerpt: null });
  expect(() => parseRow(schema, "articles", prepared)).toThrow(
    "articles.id: Missing field",
  );
  const row = parseRow(schema, "articles", { ...prepared, id: articleId });
  expect(String(row.id)).toBe(articleId.toLowerCase());
  expect(
    parsePatch(schema, "articles", {
      excerpt: "Preview",
      ordinal: -(1n << 63n),
    }),
  ).toEqual({ excerpt: "Preview", ordinal: -(1n << 63n) });
  expect(parsePatch(schema, "articles", { title: undefined, excerpt: null })).toEqual({ excerpt: null });
  expect(() => parsePatch(schema, "articles", { unknown: undefined })).toThrow("Unknown field");
  expect(() => parsePatch(schema, "articles", { id: undefined })).toThrow("Primary key assignment forbidden");
  expect(() => parsePatch(schema, "articles", { id: articleId })).toThrow(
    "Primary key assignment forbidden",
  );
  expect(() =>
    prepareInsert(schema, "articles", { ...draft, id: articleId }),
  ).toThrow("Primary key assignment forbidden");
  expect(() =>
    prepareInsert(schema, "articles", { publication, title: "First" }),
  ).toThrow("articles.excerpt: Missing field");
  expect(() =>
    prepareInsert(schema, "articles", { ...draft, published: null }),
  ).toThrow("articles.published");
  expect(() => parsePatch(schema, "articles", { ordinal: 1 })).toThrow(
    "articles.ordinal",
  );
  expect(() => parsePatch(schema, "articles", { ordinal: 1n << 63n })).toThrow(
    "articles.ordinal",
  );
  expect(() =>
    parsePatch(schema, "articles", { ordinal: -(1n << 63n) - 1n }),
  ).toThrow("articles.ordinal");
  expect(() => parsePatch(schema, "articles", { unknown: true })).toThrow(
    "Unknown field",
  );
  expect(() =>
    parseRow(schema, "articles", { ...row, id: "not-a-uuid" }),
  ).toThrow("articles.id");
  expect(() =>
    prepareInsert(schema, "publications", { title: "Daily" }),
  ).toThrow("publications.slug: Missing field");
});

test("generation inherits schema defaults and honors explicit generators and insert policies", () => {
  const declared = defineSchema(
    {
      inherited: defineTable({}),
      override: defineTable({}, { keyGeneration: "uuidV7" }),
      manual: defineTable({ id: v.uuid() }, { keyGeneration: "none" }),
      custom: defineTable(
        { id: v.int64({ mode: "bigint" }), note: v.string().nullable().default(null) },
        {
          keyGeneration: { handler: "next", outputType: "int64" },
        },
      ),
    },
    { defaultUuidGeneration: "v4" },
  );
  expect(compileSchema(declared).map((table) => table.keyGeneration)).toEqual([
    "uuidV4",
    "uuidV7",
    "none",
    { handler: "next", outputType: "int64" },
  ]);
  expect(prepareInsert(declared, "inherited", {})).toEqual({});
  expect(() => prepareInsert(declared, "manual", {})).toThrow("Missing field");
  expect(prepareInsert(declared, "custom", {})).toEqual({ note: null });
  expect(
    prepareInsert(declared, "custom", { id: 4n, note: "Provided" }),
  ).toMatchObject({ id: 4n, note: "Provided" });
});

test("schema declarations snapshot mutable authoring options before caching contracts", () => {
  const generator = { handler: "next", outputType: "int64" as const };
  const fields: ("id" | "value")[] = ["id", "value"];
  const declared = defineSchema({
    counters: defineTable(
      { id: v.int64({ mode: "bigint" }), value: v.string() },
      {
        keyGeneration: generator,
        unique: { counter: { fields } },
      },
    ),
  });
  generator.handler = "changed";
  fields.pop();
  const table = compileSchema(declared)[0]!;
  expect(table.keyGeneration).toEqual({ handler: "next", outputType: "int64" });
  expect(table.unique.counter!.fields).toEqual(["id", "value"]);
  expect(prepareInsert(declared, "counters", { value: "x" })).toEqual({
    value: "x",
  });
});

test("invalid authored schemas fail before code generation with field or constraint paths", () => {
  const invalidTable = (
    fields: Parameters<typeof defineTable>[0],
    options: TableOptions,
    diagnostic: string,
  ) => {
    expect(() =>
      defineSchema({ articles: defineTable(fields, options) }),
    ).toThrow(diagnostic);
  };
  invalidTable({ id: v.boolean() }, {}, "articles.id");
  invalidTable({ id: v.uuid().nullable() }, {}, "articles.id");
  invalidTable({ id: v.uuid().default(articleId) }, {}, "articles.id");
  invalidTable(
    { slug: v.string() },
    { primaryKey: "missing" },
    "articles.missing",
  );
  invalidTable({ id: v.string() }, { keyGeneration: "uuidV7" }, "output type");
  invalidTable(
    { id: v.string() },
    { insertKeyPolicy: "generatedOnly" },
    "need a generator",
  );
  invalidTable(
    { id: v.int64({ mode: "bigint" }) },
    { keyGeneration: { handler: "", outputType: "int64" } },
    "custom generator",
  );
  invalidTable(
    { title: v.string() },
    { unique: { title: { fields: ["title", "title"] } } },
    "articles.title",
  );
  invalidTable(
    { title: v.string() },
    { unique: { title: { fields: ["missing"] } } },
    "Unknown unique field",
  );
  invalidTable(
    { title: v.string() },
    {
      unique: {
        first: { fields: ["title", "id"] },
        second: { fields: ["id", "title"] },
      },
    },
    "Duplicate unique field set",
  );
  invalidTable(
    { title: v.string().nullable() },
    { unique: { title: { fields: ["title"] } } },
    "null policy",
  );
  expect(() =>
    defineSchema({
      articles: defineTable({ publication: v.id("missing") }),
    }),
  ).toThrow("articles.publication");
  expect(() => defineSchema({ articles: defineTable({ publication: v.id("publications", {onDelete:"setNull"}) }), publications:defineTable({}) })).toThrow("nullable");
});

test("forward and cyclic references resolve keys without assuming local target rows exist", () => {
  const declared = defineSchema({
    left: defineTable({ right: v.id("right").nullable() }),
    right: defineTable({ left: v.id("left") }),
  });
  expect(compileSchema(declared)).toHaveLength(2);
  expect(prepareInsert(declared, "right", { left: articleId })).toMatchObject({
    left: articleId.toLowerCase(),
  });
});

test("record validation accepts plain records and rejects prototype or unknown-field payloads", () => {
  expect(
    prepareInsert(
      schema,
      "articles",
      Object.assign(Object.create(null), draft),
    ),
  ).toMatchObject(draft);
  expect(() => prepareInsert(schema, "articles", [])).toThrow("plain object");
  expect(() => prepareInsert(schema, "articles", Object.create(draft))).toThrow(
    "plain object",
  );
  expect(() =>
    prepareInsert(
      schema,
      "articles",
      JSON.parse(
        '{"publication":"daily","title":"x","excerpt":null,"__proto__":{}}',
      ),
    ),
  ).toThrow("Unknown field");
  const special = defineSchema({
    ["__proto__"]: defineTable({ ["__proto__"]: v.string() }),
  });
  expect(
    Object.hasOwn(
      prepareInsert(special, "__proto__", JSON.parse('{"__proto__":"value"}')),
      "__proto__",
    ),
  ).toBe(true);
  // Rust strings cannot represent unpaired UTF-16 surrogate code units.
  expect(() => v.string().default("\ud800")).toThrow("Expected a string");
  expect(() =>
    prepareInsert(schema, "articles", { ...draft, title: "\udfff" }),
  ).toThrow("articles.title");
});

test("schema and function generation coexist and protect all authored destinations", async () => {
  const directory = await mkdtemp(
    new URL("./schema-codegen-", import.meta.url).pathname,
  );
  try {
    const rustOutput = resolve(directory, "native", "schema.rs");
    await generateSchema({ schema: source, outDir: directory, rustOutput });
    const types = await readFile(resolve(directory, "types.ts"), "utf8");
    await generateFunctions({
      modules: {
        notes: new URL("./fixtures/notes.ts", import.meta.url).pathname,
      },
      outDir: directory,
    });
    expect(await readFile(resolve(directory, "types.ts"), "utf8")).toBe(types);
    const authored = "// Handwritten Rust\n";
    await writeFile(rustOutput, authored);
    await rm(resolve(directory, "types.ts"));
    await expect(
      generateSchema({ schema: source, outDir: directory, rustOutput }),
    ).rejects.toThrow("Refusing to overwrite");
    await expect(access(resolve(directory, "types.ts"))).rejects.toThrow();
    expect(await readFile(rustOutput, "utf8")).toBe(authored);
  } finally {
    await rm(directory, { recursive: true });
  }
});

test("JSON replacement validates strict nested objects and optional members", () => {
  const schema = defineSchema({ records: defineTable({
    data: v.object({ items: v.array(v.object({ name: v.string(), score: v.number().nullable().optional() })) }),
    tags: v.array(v.string()).nullable(),
    open: v.json<{ anything: string }>().nullable(),
    ratio: v.number(),
  }) });
  const input = { tags: null, data: { items: [{ name: "first" }, { name: "second", score: null }] }, open: { anything: "ok" }, ratio: 1e30 };
  expect(prepareInsert(schema, "records", input)).toEqual(input);
  expect(parsePatch(schema, "records", { tags: ["one"] })).toEqual({ tags: ["one"] });
  expect(() => parsePatch(schema, "records", { tags: [1] })).toThrow();
  expect(parsePatch(schema, "records", { data: { items: [] } })).toEqual({ data: { items: [] } });
  expect(() => parsePatch(schema, "records", { data: { items: [{ name: "ok", extra: true }] } })).toThrow("$.items[0].extra");
  expect(() => parsePatch(schema, "records", { data: { items: [{ name: "ok", score: undefined }] } })).toThrow();
  expect(() => parsePatch(schema, "records", { data: null })).toThrow();
  expect(() => parsePatch(schema, "records", { ratio: Infinity })).toThrow();
  expect(parsePatch(schema, "records", { data: undefined, open: null })).toEqual({ open: null });
});

test("unrestricted JSON rejects lossy serialization and never invokes accessors", () => {
  const field = v.json();
  expect(v.json<{ claimed: string }>().parse({ unrelated: 1 }) as unknown).toEqual({ unrelated: 1 });
  const cycle: unknown[] = []; cycle.push(cycle);
  let called = false;
  const accessor = { get value() { called = true; return 1; } };
  for (const value of [new Date(), { value: 1n }, { value: undefined }, [undefined], new Array(1), cycle, accessor, { toJSON() { return 1; } }, { value: NaN }]) expect(() => field.parse(value)).toThrow();
  expect(called).toBe(false);
  expect(field.parse({ value: 1e30, nested: [null, true] })).toEqual({ value: 1e30, nested: [null, true] });
});


test("literal dotted column and JSON property names survive validation", () => {
  const schema = defineSchema({ records: defineTable({ "data.value": v.json({ "nested.value": v.string() }) }) });
  const input = { "data.value": { "nested.value": "literal" } };
  expect(prepareInsert(schema, "records", input)).toEqual(input);
  expect(() => parsePatch(schema, "records", { "data.value": { nested: { value: "wrong" } } } as unknown)).toThrow();
});
