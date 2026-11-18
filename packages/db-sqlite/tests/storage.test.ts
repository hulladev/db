import { initializeDatabase, connectDatabase } from "./fixtures/open.ts";
import { expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { defineSchema, defineTable, v, sql } from "@hulla/db";

const schema = defineSchema({
  notes: defineTable({ title: v.string().min(1), count: v.int64({ mode: "bigint" }).default(0n), done: v.boolean().default(false), extra: v.object({ tags: v.array(v.string()) }).nullable() }),
  private: defineTable({ value: v.string() }),
});
const definition = schema;
const id = "019a0000-0000-7000-8000-000000000001";
const otherId = "019a0000-0000-7000-8000-000000000002";
const insert = { id, title: "Original", count: 9_223_372_036_854_775_806n, extra: { tags: ["one", "two"] } };

test("SQLite requires explicit initialization and preserves exact values and failed transactions across reopen", async () => {
  const directory = await mkdtemp(join(tmpdir(), "hulla-sqlite-"));
  const filename = join(directory, "local.sqlite");
  let db;
  try {
    await expect(connectDatabase(definition, { filename })).rejects.toMatchObject({ code: "database_uninitialized" });
    db = await initializeDatabase(definition, { filename });
    const note = await db.insert("notes", insert);
    expect(note).toEqual({ ...insert, done: false });
    expect(await db.select("notes", ["count"]).where("count", ">", 9_223_372_036_854_775_805n)).toEqual([{ count: insert.count }]);
    expect(await db.select("notes").aggregate(a => ({ count: a.count(), largest: a.max("count") }))).toEqual({ count: 1, largest: insert.count });
    await expect(db.transaction(async tx => {
      await tx.insert("private", { value: "must roll back" });
      try { await tx.update("notes", { title: "Changed" }).where({ id }).expectRows(2); } catch {}
    })).rejects.toMatchObject({ code: "row_expectation_failed" });
    expect(await db.select("private")).toEqual([]);
    expect((await db.select("notes").unique())?.title).toBe("Original");
    await expect(db.transaction(async tx => { await tx.update("notes", current => ({ count: current.count.add(2n) })).where({ id }); })).rejects.toThrow();
    await db.close();
    db = await connectDatabase(definition, { filename });
    expect(await db.select("notes").unique()).toEqual(note);
    const incompatible = defineSchema({ notes: defineTable({ title: v.number() }) });
    await expect(connectDatabase(incompatible, { filename })).rejects.toMatchObject({ code: "schema_mismatch" });
    expect(await db.select("notes").unique()).toEqual(note);
  } finally { await db?.close(); await rm(directory, { recursive: true, force: true }); }
});

test("SQLite enforces references, checks, null uniqueness and atomic bulk upserts", async () => {
  const local = defineSchema({
    owners: defineTable({ name: v.string().min(2) }),
    contacts: defineTable({ owner: v.id("owners"), email: v.string().nullable(), visits: v.int64({ mode: "bigint" }) }, {
      unique: { email: { fields: ["email"], nulls: "notDistinct" } }, checks: fields => ({ positive: sql`${fields.visits} >= ${0n}` }),
    }),
    children: defineTable({ owner: v.id("owners", { onDelete: "cascade" }), name: v.string() }),
  });
  const db = await initializeDatabase(local, { filename: ":memory:" });
  try {
    const owner = await db.insert("owners", { name: "Ada" });
    expect(owner.id[14]).toBe("7");
    await expect(Promise.resolve(db.insert("contacts", { owner: id, email: "bad", visits: 1n }))).rejects.toThrow();
    await db.insert("contacts", { owner: owner.id, email: null, visits: 1n });
    await expect(Promise.resolve(db.insert("contacts", { owner: owner.id, email: null, visits: 1n }))).rejects.toThrow();
    await expect(Promise.resolve(db.insert("contacts", { owner: owner.id, email: "negative", visits: -1n }))).rejects.toThrow();
    expect(await db.insert("contacts", { owner: owner.id, email: null, visits: 2n }).onConflict(({ on }) => on("email").update(({ current, incoming }) => ({ visits: current.visits.add(incoming.visits) })))).toMatchObject({ visits: 3n });
    await expect(Promise.resolve(db.insert("contacts", [
      { owner: owner.id, email: "duplicate", visits: 1n }, { owner: owner.id, email: "duplicate", visits: 1n },
    ]).onConflict(({ on }) => on("email").update({ visits: 9n })))).rejects.toMatchObject({ code: "cardinality_violation" });
    expect(await db.select("contacts").where({ email: "duplicate" })).toEqual([]);
    await db.insert("children", { owner: owner.id, name: "Child" });
    await expect(Promise.resolve(db.delete("owners").where({ id: owner.id }))).rejects.toThrow();
    await db.transaction(async tx => { await tx.delete("contacts").where(true); await tx.delete("owners").where(true); });
    expect(await db.select("children")).toEqual([]);
  } finally { await db.close(); }
});

test("SQLite filters preserve null semantics, exact int64 ordering and literal text matching", async () => {
  const local = defineSchema({ items: defineTable({ title: v.string().nullable(), n: v.int64({ mode: "bigint" }).nullable() }) });
  const db = await initializeDatabase(local, { filename: ":memory:" });
  try {
    await db.insert("items", [{ id, title: "a%_\\z", n: 10n }, { id: otherId, title: null, n: null }, { title: "abc", n: -3n }]);
    expect((await db.select("items").where("title", "contains", "%_\\")).map(row => row.id)).toEqual([id]);
    expect(await db.select("items").where("title", "not endsWith", "")).toEqual([]);
    expect(await db.select("items").where(({ exists }) => exists("title", false))).toEqual([]);
    expect((await db.select("items").where("n", "in", [null, 10n])).map(row => row.id)).toEqual([id, otherId]);
    expect((await db.select("items").orderBy("n")).map(row => row.n)).toEqual([-3n, 10n, null]);
    expect((await db.select("items").orderBy("n", "desc")).map(row => row.n)).toEqual([null, 10n, -3n]);
    await expect(Promise.resolve(db.select("items").where("title", "contains", "a", { caseSensitive: false }))).rejects.toMatchObject({ code: "unsupported_query" });
  } finally { await db.close(); }
});

test("SQLite rejects escaped handles, nested root access and unawaited writes", async () => {
  const db = await initializeDatabase(definition, { filename: ":memory:" });
  let escaped: Parameters<Parameters<typeof db.transaction>[0]>[0] | undefined;
  try {
    await db.transaction(async tx => {
      escaped = tx;
      await expect(Promise.resolve(db.select("notes"))).rejects.toMatchObject({ code: "transaction_scope" });
      await expect(db.transaction(() => 0)).rejects.toMatchObject({ code: "transaction_scope" });
      await Promise.all([tx.insert("private", { value: "first" }), tx.insert("private", { value: "second" })]);
    });
    await expect(Promise.resolve(escaped!.insert("private", { value: "escaped" }))).rejects.toMatchObject({ code: "transaction_closed" });
    await expect(db.transaction(tx => { void tx.insert("private", { value: "unawaited" }).then(() => {}); })).rejects.toMatchObject({ code: "unawaited_operations" });
    expect(await db.select("private")).toHaveLength(2);
  } finally { await db.close(); }
  await expect(Promise.resolve(db.select("private"))).rejects.toMatchObject({ code: "disposed" });
});

test("transaction deadlines roll back writes and invalidate late callbacks", async () => {
  const db = await initializeDatabase(definition, { filename: ":memory:", transactionTimeoutMs: 15 });
  const barrier = Promise.withResolvers<void>();
  let late!: Promise<unknown>;
  try {
    const transaction = db.transaction(async tx => {
      await tx.insert("private", { value: "rolled back on timeout" });
      late = barrier.promise.then(() => Promise.resolve(tx.insert("private", { value: "too late" })));
      await barrier.promise;
    });
    await expect(transaction).rejects.toMatchObject({ code: "transaction_timeout" });
    expect(await db.select("private")).toEqual([]);
    barrier.resolve();
    await expect(late).rejects.toMatchObject({ code: "transaction_closed" });
  } finally { barrier.resolve(); await db.close(); }
});
