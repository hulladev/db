import assert from "node:assert/strict";
import { defineSchema, defineTable, v } from "@hulla/db";
import { bunSQLiteDriver } from "@hulla/db-sqlite/bun";
import { sqlite } from "@hulla/db-sqlite/expo";

Object.defineProperty(globalThis, "crypto", { configurable: true, value: undefined });
Bun.plugin({ name: "Expo secure entropy", setup(builder) {
  builder.module("expo-crypto", () => ({
    loader: "object",
    exports: { getRandomValues: (array: Uint8Array) => array.fill(0xa5) },
  }));
} });

const schema = defineSchema({ tasks: defineTable({ title: v.string() }) });
const db = sqlite.initialize({ schema, open: () => bunSQLiteDriver(":memory:") });
try {
  const row = await db.insert("tasks", { title: "Draft" });
  // The entropy survives in the UUID alongside the required version and variant bits.
  assert.match(row.id, /^[0-9a-f]{8}-[0-9a-f]{4}-75a5-a5a5-a5a5a5a5a5a5$/);
  assert.deepEqual(await db.select("tasks"), [row]);
} finally { await db.close(); }
