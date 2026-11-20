import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { defineSchema, defineTable, v } from "@hulla/db";
import { sqlite } from "@hulla/db-expo";
import { bunSQLiteDriver, sqlite as bunSQLite } from "@hulla/db-sqlite/bun";
import type { SQLiteValue } from "@hulla/db-sqlite/portable";

const schema = defineSchema({ tasks: defineTable({ title: v.string() }) });
const directory = await mkdtemp(join(tmpdir(), "hulla-expo-adapter-"));
const filename = join(directory, "journal.sqlite");
let opens = 0, closes = 0;

Bun.plugin({ name: "Expo native modules", setup(builder) {
  builder.module("expo-crypto", () => ({
    loader: "object",
    exports: { getRandomValues: (array: Uint8Array) => array.fill(0xa5) },
  }));
  builder.module("expo-sqlite", () => ({
    loader: "object",
    exports: {
      async openDatabaseAsync(file: string, options: { useNewConnection: boolean }) {
        assert.equal(file, filename);
        assert.equal(options.useNewConnection, true);
        const fail = ++opens === 1;
        const driver = await bunSQLiteDriver(file);
        return {
          async getAllAsync(sql: string, parameters: SQLiteValue[]) {
            if (fail) throw Object.assign(new Error("Error code 261: database is locked"), { code: "ERR_INTERNAL_SQLITE_ERROR" });
            return driver.execute(sql, parameters);
          },
          async closeAsync() { await driver.close(); closes++; },
        };
      },
    },
  }));
} });

try {
  const seed = bunSQLite.initialize({ schema, filename });
  const existing = await seed.insert("tasks", { title: "Existing" }).finally(() => seed.close());
  Object.defineProperty(globalThis, "crypto", { configurable: true, value: undefined });
  const db = sqlite({ schema, filename });
  try {
    assert.deepEqual(await db.select("tasks"), [existing]);
    assert.deepEqual([opens, closes], [2, 1]);
    const created = await db.insert("tasks", { title: "Draft" });
    assert.match(created.id, /^[0-9a-f]{8}-[0-9a-f]{4}-75a5-a5a5-a5a5a5a5a5a5$/);
  } finally { await db.close(); }
  assert.equal(closes, 2);
} finally { await rm(directory, { recursive: true, force: true }); }
