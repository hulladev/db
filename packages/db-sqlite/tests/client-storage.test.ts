import { expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { defineSchema, defineTable, v } from "@hulla/db";
import { openSQLiteDatabase } from "../src/runtime.ts";
import { bunSQLiteDriver } from "../src/bun-driver.ts";
import { schemaIdentity } from "../../db/src/database-metadata.ts";
import { clientStorage } from "../../db/src/client-storage.ts";
import type { QueuedMutation } from "../../db/src/client-storage.ts";

const primary = defineSchema({ notes: defineTable({ title: v.string(), count: v.int64({ mode: "bigint" }), secret: v.string() }) });
const device = defineSchema({ notes: primary.tables.notes.omit("secret").replica(), drafts: defineTable({ title: v.string() }) });
const source = "00000000-0000-0000-0000-000000000001", scope = "00000000-0000-0000-0000-000000000002", principal = "00000000-0000-0000-0000-000000000003", authorizationScope = "00000000-0000-0000-0000-000000000004";
const owner = { source, scope, principal, authorizationScope };
const key = '["alice"]', rowId = "00000000-0000-0000-0000-000000000005";
const contract = { table: "notes", columns: ["id", "title", "count"] };
const snapshot = (revision: string, result: unknown) => ({ source, scope, principal, authorization_scope: authorizationScope, revision, result });

test("replica storage preserves overlap, exact scalars, account fences and queued evidence across reopen", async () => {
  const directory = await mkdtemp(join(tmpdir(), "hulla-client-storage-"));
  const filename = join(directory, "device.sqlite");
  let db = await openSQLiteDatabase(device, await bunSQLiteDriver(filename), {}, true);
  try {
    let store = clientStorage(db, schemaIdentity(primary));
    await expect(Promise.resolve(db.select("notes"))).rejects.toMatchObject({ code: "replica_scope_uninitialized" });
    await store.activate(key); await store.bind(key, owner);
    const row = { id: rowId, title: "First", count: 9223372036854775806n };
    await store.saveSnapshot(key, "workspace", snapshot("1", [{ ...row, count: String(row.count) }]), contract, [row]);
    await store.saveSnapshot(key, "assigned", snapshot("2", [{ ...row, title: "Newer", count: String(row.count) }]), contract, [{ ...row, title: "Newer" }]);
    expect(await db.select("notes")).toEqual([{ ...row, title: "Newer" }]);
    // @ts-expect-error exercise the runtime fence against dynamically typed writes
    await expect(Promise.resolve(db.update("notes", { title: "Unsynced" }).where(true))).rejects.toMatchObject({ code: "replica_read_only" });
    await store.forgetSnapshot(key, "workspace");
    expect(await db.select("notes")).toEqual([{ ...row, title: "Newer" }]);
    const queued: QueuedMutation = { id: crypto.randomUUID(), name: "notes.rename", input: { title: "Pending" }, owner, attempt: 1, state: "queued", requestId: crypto.randomUUID() };
    await store.enqueue(key, queued);
    const sent: QueuedMutation = { ...queued, state: "sent", ticket: crypto.randomUUID() };
    expect(await store.update(key, sent, queued)).toBe(true);
    expect(await store.update(key, { ...queued, state: "failed" }, queued)).toBe(false);
    await expect(store.update(key, { ...sent, input: "x".repeat(16 * 1024 * 1024) }, sent)).rejects.toMatchObject({ code: "capacity" });
    expect(await store.mutations(key)).toEqual([sent]);
    await db.close();
    db = await openSQLiteDatabase(device, await bunSQLiteDriver(filename));
    store = clientStorage(db, schemaIdentity(primary));
    expect(await store.mutations(key)).toEqual([sent]);
    await expect(Promise.resolve(db.select("notes"))).rejects.toMatchObject({ code: "replica_scope_uninitialized" });
    await store.activate('["bob"]');
    expect(await db.select("notes")).toEqual([]);
    expect(await store.mutations(key)).toEqual([sent]);
    await store.activate(key);
    expect(await db.select("notes")).toEqual([{ ...row, title: "Newer" }]);
    store.deactivate();
    await expect(Promise.resolve(db.select("notes"))).rejects.toMatchObject({ code: "replica_scope_uninitialized" });
    expect(await db.insert("drafts", { title: "Independent" })).toMatchObject({ title: "Independent" });
    await expect(store.bind(key, { ...owner, source: crypto.randomUUID() })).rejects.toMatchObject({ code: "reset_required" });
    expect(await store.mutations(key)).toEqual([sent]);
    await store.bind(key, { ...owner, authorizationScope: crypto.randomUUID() });
    await store.activate(key);
    expect(await db.select("notes")).toEqual([]);
    expect(await store.mutations(key)).toEqual([sent]);
  } finally { await db.close(); await rm(directory, { recursive: true, force: true }); }
});

test("independent sources retain separate active accounts on one device connection", async () => {
  const catalog = defineSchema({ products: defineTable({ title: v.string() }) });
  const mixed = defineSchema({ notes: primary.tables.notes.omit("secret").replica(), summaries: primary.tables.notes.pick("id", "title").replica(), products: catalog.tables.products.replica() });
  const db = await openSQLiteDatabase(mixed, await bunSQLiteDriver(":memory:"), {}, true);
  try {
    const notes = clientStorage(db, schemaIdentity(primary)), products = clientStorage(db, schemaIdentity(catalog));
    await notes.activate(key); await notes.bind(key, owner);
    const note = { id: rowId, title: "Private", count: 4n };
    await notes.saveSnapshot(key, "notes", snapshot("1", [{ ...note, count: "4" }]), contract, [note]);
    const summary = { id: rowId, title: "New summary" };
    await notes.saveSnapshot(key, "summaries", snapshot("2", [summary]), { table: "notes", columns: ["id", "title"] }, [summary]);
    expect(await db.select("summaries")).toEqual([summary]);
    await products.activate('["catalog"]'); await products.bind('["catalog"]', owner);
    const product = { id: rowId, title: "Public" };
    await products.saveSnapshot('["catalog"]', "products", snapshot("1", [product]), { table: "products", columns: ["id", "title"] }, [product]);
    expect(await db.select("notes")).toEqual([note]);
    expect(await db.select("products")).toEqual([product]);
    await notes.activate('["bob"]');
    expect(await db.select("notes")).toEqual([]);
    expect(await db.select("summaries")).toEqual([]);
    expect(await db.select("products")).toEqual([product]);
    notes.deactivate();
    await expect(Promise.resolve(db.select("notes"))).rejects.toMatchObject({ code: "replica_scope_uninitialized" });
    expect(await db.select("products")).toEqual([product]);
  } finally { await db.close(); }
});

test('large replica downloads batch native writes within a portable parameter budget', async () => {
  const driver = await bunSQLiteDriver(':memory:');
  const batches: number[] = [];
  const db = await openSQLiteDatabase(device, {
    execute(sql, parameters = []) {
      if (parameters.length > 999) throw new Error('Native parameter limit exceeded');
      if (sql.startsWith('INSERT INTO "_hulla_t')) batches.push(parameters.length);
      return driver.execute(sql, parameters);
    },
    close: () => driver.close(),
  }, {}, true);
  try {
    const store = clientStorage(db, schemaIdentity(primary));
    await store.activate(key); await store.bind(key, owner);
    const rows = Array.from({ length: 1100 }, (_, index) => ({ id: crypto.randomUUID(), title: `Row ${index}`, count: 9223372036854774707n + BigInt(index) }));
    const wire = rows.map(row => ({ ...row, count: String(row.count) }));
    await store.saveSnapshot(key, 'bulk', snapshot('1', wire), contract, rows);
    expect(await db.select('notes').orderBy('title')).toEqual([...rows].sort((left, right) => left.title < right.title ? -1 : 1));
    expect(batches.length).toBeLessThan(20);
    expect(Math.max(...batches)).toBeLessThanOrEqual(999);
    const survivors = rows.slice(900);
    await store.saveSnapshot(key, 'bulk', snapshot('2', wire.slice(900)), contract, survivors);
    expect(await db.select('notes')).toHaveLength(survivors.length);
    await store.forgetSnapshot(key, 'bulk');
    expect(await db.select('notes')).toEqual([]);
  } finally { await db.close(); }
});
