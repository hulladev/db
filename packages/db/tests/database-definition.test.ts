import { initializeDatabase } from "./fixtures/sqlite.ts";
import { expect, test } from "bun:test";
import { postgres } from "@hulla/db-postgres";
import { defineSchema, defineTable, sql, v } from "../src/index.ts";
import { compileSchema, parseRow, prepareInsert } from "../src/schema.ts";
import { executeDefinition, replicaSelection } from "../src/declarations.ts";
import { objectInput } from "./fixtures/standard.ts";

test("composed standalone tables have independent defaults, codecs, fields and storage bindings", async () => {
  const drafts = defineTable({ title: v.string().min(1), body: v.string(), done: v.boolean().default(false) });
  const publications = drafts.extend({ body: v.string().nullable().default(null), publishedAt: v.timestamp() });
  const summaries = publications.pick("title", "publishedAt");
  const schema = defineSchema({ drafts, publications, summaries });
  const database = schema;
  const db = await initializeDatabase(database, { filename: ":memory:" });
  try {
    const publishedAt = new Date("2026-11-05T12:00:00.000Z");
    const publication = await db.insert("publications", { title: "Published", publishedAt });
    expect(publication).toMatchObject({ title: "Published", body: null, done: false, publishedAt });
    expect(await db.select("drafts")).toEqual([]);
    const summary = await db.insert("summaries", { title: "Short", publishedAt });
    expect(Object.keys(summary).sort()).toEqual(["id", "publishedAt", "title"]);
    expect(prepareInsert(schema, "drafts", { title: "Draft", body: "Text" })).toEqual({ title: "Draft", body: "Text", done: false });
    expect(() => prepareInsert(schema, "drafts", { title: "", body: "Text" })).toThrow();
    expect(() => prepareInsert(schema, "drafts", { title: "Draft" })).toThrow("Missing field");
    expect(Object.keys(drafts.fields)).toEqual(["title", "body", "done"]);
    expect(Object.keys(publications.omit("body").fields)).toEqual(["title", "done", "publishedAt"]);
  } finally { await db.close(); }
});

test("composition rejects removed key/constraint columns and incompatible overrides without weakening its source", () => {
  const constrained = defineTable({ slug: v.string(), body: v.string() }, {
    primaryKey: "slug", unique: { body: { fields: ["body"] } },
  });
  expect(() => constrained.omit("slug")).toThrow("primary key");
  expect(() => constrained.pick("body")).toThrow("primary key");
  expect(() => constrained.omit("body")).toThrow("unique constraint");
  expect(() => constrained.extend({ body: v.string().nullable() })).toThrow("null policy");
  const indexed = defineTable({ body: v.string() }).index(["body"]);
  expect(() => indexed.omit("body")).toThrow("index");
  const checked = defineTable({ low: v.number(), high: v.number() }, { checks: f => ({ range: sql`${f.low} <= ${f.high}` }) });
  expect(() => checked.omit("high")).toThrow();
  const generated = defineTable({ id: v.uuid(), body: v.string() }, { keyGeneration: "uuidV7" });
  expect(() => generated.extend({ id: v.int64() })).toThrow("generator output");
  expect(() => (constrained.omit as (...fields: string[]) => unknown)("unknown")).toThrow("Unknown projection field");
  expect(() => (constrained.pick as (...fields: string[]) => unknown)()).toThrow("distinct fields");
  expect(compileSchema(defineSchema({ constrained }))[0]!.unique).toHaveProperty("body");
});

test("portable templates bind independently with different names and UUID defaults", () => {
  const workspace = defineTable({ name: v.string() });
  const task = defineTable({ workspace: v.id(workspace), title: v.string() });
  const first = defineSchema({ workspace, task }, { defaultUuidGeneration: "v4" });
  const second = defineSchema({ groups: workspace, tasks: task }, { defaultUuidGeneration: "v7" });
  expect(compileSchema(first).find(table => table.name === "task")!.fields.workspace!.target).toBe("workspace");
  expect(compileSchema(second).find(table => table.name === "tasks")!.fields.workspace!.target).toBe("groups");
  expect(compileSchema(first)[0]!.keyGeneration).toBe("uuidV4");
  expect(compileSchema(second)[0]!.keyGeneration).toBe("uuidV7");
});

test("bound projections retain source identity, resolve references and exclude source-only constraints", () => {
  const workspaces = defineTable({ slug: v.string() }, { primaryKey: "slug" });
  const tasks = defineTable({ workspace: v.id(workspaces), title: v.string(), internalNotes: v.string() }, {
    unique: { internal: { fields: ["workspace", "internalNotes"] } },
    checks: f => ({ internal: sql`${f.internalNotes} <> ${""}` }),
  }).index(["internalNotes"]);
  const schema = defineSchema({ workspaces, tasks });
  const primary = schema;
  const independent = defineSchema({ workspaces, tasks });
  const publicTasks = primary.tables.tasks.omit("internalNotes");
  const device = defineSchema({
    tasks: publicTasks.replica(),
    selected: defineTable({ task: v.id(primary.tables.tasks), workspace: v.id(primary.tables.workspaces) }),
    spaces: primary.tables.workspaces.replica(),
  });
  const compiled = compileSchema(device);
  const replicated = compiled.find(table => table.name === "tasks")!;
  expect(Object.keys(replicated.fields).sort()).toEqual(["id", "title", "workspace"]);
  expect(replicated.fields.workspace!.scalar).toBe("string");
  expect(replicated.fields.workspace!.target).toBeUndefined();
  expect(replicated.unique).toEqual({});
  expect(replicated.checks).toEqual([]);
  expect(replicated.indexes).toEqual([]);
  expect(compiled.find(table => table.name === "selected")!.fields.workspace!.target).toBe("spaces");
  expect(() => (publicTasks.omit as (...fields: string[]) => unknown)("id")).toThrow("primary key");
  expect(() => (publicTasks.pick as (...fields: string[]) => unknown)("title")).toThrow("primary key");
  expect(() => defineSchema({ tasks: primary.tables.tasks })).toThrow(".replica()");
  expect(() => postgres({ schema: device, url: 'postgres://localhost/test', sources: [] } as never)).toThrow("replica storage");
  expect(() => device.tables.tasks.replica()).toThrow("authoritative");
  expect(() => independent.query({ replica: publicTasks, handler: ({ db }) => db.select("tasks", ["id", "workspace", "title"]) })).toThrow("own schema");
  expect(parseRow(device, "tasks", { id: "aaaaaaaa-0000-0000-0000-000000000001", title: "Local", workspace: "w" })).toMatchObject({ workspace: "w" });
});

test("replica queries authorize genuine source selections and reject forged, widened or modified results", async () => {
  const primary = defineSchema({ tasks: defineTable({
    workspace: v.string(), title: v.string(), internalNotes: v.string(),
  }), other: defineTable({ workspace: v.string(), title: v.string() }) });
  const publicTasks = primary.tables.tasks.omit("internalNotes");
  const db = await initializeDatabase(primary, { filename: ":memory:" });
  try {
    await db.insert("tasks", { workspace: "alice", title: "Allowed", internalNotes: "PRIVATE" });
    await db.insert("tasks", { workspace: "bob", title: "Forbidden", internalNotes: "SECRET" });
    const independent = defineSchema({ tasks: defineTable({ workspace: v.string(), title: v.string(), internalNotes: v.string() }), other: defineTable({ workspace: v.string(), title: v.string() }) });
    expect(() => db.select(independent.tables.tasks)).toThrow("different database");
    const authenticated = primary.query.extend(({ identity }) => {
      if (typeof identity !== "string") throw new Error("Sign in required");
      return { workspace: identity };
    });
    const query = authenticated({
      replica: publicTasks, input: objectInput({ workspace: v.string() }),
      handler: async (ctx, input) => {
        if (ctx.workspace !== input.workspace) throw new Error("Forbidden");
        return ctx.db.select(publicTasks).where({ workspace: input.workspace });
      },
    });
    await db.transaction(async tx => {
      expect(await tx.select(primary.tables.tasks).aggregate(a => ({ count: a.count() }))).toEqual({ count: 2 });
      const titles = primary.tables.tasks.pick("id", "title");
      expect(await tx.select(titles).where({ workspace: "alice" })).toEqual([expect.objectContaining({ title: "Allowed" })]);
      const result = await executeDefinition(query, { db: tx, identity: "alice" }, { workspace: "alice" });
      expect(result).toEqual([expect.objectContaining({ title: "Allowed", workspace: "alice" })]);
      expect(JSON.stringify(result)).not.toContain("PRIVATE");
      const selection = replicaSelection(query, result);
      expect(selection.table).toBe("tasks");
      expect(selection.columns).not.toContain("internalNotes");
      expect(selection.filters).toHaveLength(1);
      await expect(executeDefinition(query, { db: tx, identity: "alice" }, { workspace: "bob" })).rejects.toThrow("Forbidden");
      await expect(executeDefinition(query, { db: tx, identity: null }, { workspace: "alice" })).rejects.toThrow("Sign in required");
      const mapped = primary.query({ replica: publicTasks, handler: async ({ db }) => (await db.select(publicTasks)).map(row => ({ ...row })) });
      await expect(executeDefinition(mapped, { db: tx }, undefined)).rejects.toThrow("row selection");
      const widened = primary.query({ replica: publicTasks, handler: ({ db }) => db.select("tasks") });
      await expect(executeDefinition(widened, { db: tx }, undefined)).rejects.toThrow("source and columns");
      const wrongTable = primary.query({ replica: publicTasks, handler: ({ db }) => db.select("other") });
      await expect(executeDefinition(wrongTable, { db: tx }, undefined)).rejects.toThrow("source and columns");
      const modified = primary.query({ replica: publicTasks, handler: async ({ db }) => {
        const rows = await db.select(publicTasks);
        rows[0]!.title = "Forged";
        return rows;
      } });
      await expect(executeDefinition(modified, { db: tx }, undefined)).rejects.toThrow("modified");
      const foreignBinding = primary.query({ replica: publicTasks, handler: ({ db }) => db.select(independent.tables.tasks.omit("internalNotes")) });
      await expect(executeDefinition(foreignBinding, { db: tx }, undefined)).rejects.toThrow("different database");
      const other = defineSchema({ tasks: defineTable({ title: v.string() }) });
      expect(() => tx.select(other.tables.tasks as never)).toThrow("different database");
      expect(() => tx.select(publicTasks, ["internalNotes"])).toThrow("projection");
    });
    expect(() => primary.query({ replica: publicTasks, handler: () => [], output: objectInput({ title: v.string() }) } as never)).toThrow("outputs");
    expect(() => primary.mutation({ replica: publicTasks, handler: () => [] } as never)).toThrow("Unknown");
  } finally { await db.close(); }
});
