import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { defineSchema, defineTable, v } from "@hulla/db";
import { sqlite } from "@hulla/db-sqlite";
import { nodeSQLiteDriver } from "../../src/node-driver.ts";

const directory = await mkdtemp(join(tmpdir(), "hulla-node-sqlite-"));
const filename = join(directory, "db.sqlite");
const definition = defineSchema({ items: defineTable({
  value: v.int64({ mode: "bigint" }),
  label: v.string().codec<{ text: string }>({ encode: value => value.text, decode: text => ({ text }) }),
}) });
let db;
try {
  const settings = { open: () => nodeSQLiteDriver(filename) };
  db = sqlite.initialize({ schema: definition, ...settings });
  await db.ready();
  const row = await db.insert("items", { value: 9223372036854775806n, label: { text: "Portable" } });
  await db.transaction(async tx => { await tx.update("items", current => ({ value: current.value.add(1n) })).where({ id: row.id }); });
  await assert.rejects(db.transaction(async tx => {
    await tx.update("items", { label: { text: "must roll back" } }).where({ id: row.id });
    try { await tx.insert("items", { id: row.id, value: 0n, label: { text: "Duplicate" } }); } catch {}
  }));
  const publicItems = definition.tables.items.omit("label");
  assert.deepEqual(await db.select(publicItems), [{ id: row.id, value: 9223372036854775807n }]);
  await db.close();
  db = sqlite({ schema: definition, ...settings });
  await db.ready();
  assert.deepEqual(await db.select("items").unique(), { ...row, value: 9223372036854775807n });
} finally { await db?.close(); await rm(directory, { recursive: true, force: true }); }
