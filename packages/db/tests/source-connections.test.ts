import { initializeDatabase, connectDatabase } from "./fixtures/sqlite.ts";
import { expect, test } from "bun:test";
import { defineSchema, defineTable, v } from "../src/index.ts";
import { schemaIdentity } from "../src/database-metadata.ts";

test("missing, extra, duplicate and structurally identical wrong sources fail before opening a driver", async () => {
  const schema = defineSchema({ tasks: defineTable({ title: v.string() }) });
  const primary = schema;
  const sameShape = defineSchema({ tasks: defineTable({ title: v.string() }) });
  const catalog = defineSchema({ products: defineTable({ title: v.string() }) });
  const device = defineSchema({ tasks: primary.tables.tasks.replica(), products: catalog.tables.products.replica() });
  let opens = 0;
  const open = () => { opens++; throw new Error("Driver must not open"); };
  const endpoint = (database: typeof primary | typeof catalog) => ({ database: schemaIdentity(database), url: "https://api.example.com" });
  await expect(connectDatabase(device, { open } as never)).rejects.toThrow("Missing source connection");
  await expect(connectDatabase(device, { open, sources: [endpoint(primary)] } as never)).rejects.toThrow("products");
  await expect(initializeDatabase(device, { open, sources: [] } as never)).rejects.toThrow("Missing source connection");
  await expect(connectDatabase(device, { open, sources: [endpoint(primary), endpoint(catalog), endpoint(primary)] })).rejects.toThrow("Duplicate source");
  await expect(connectDatabase(device, { open, sources: [endpoint(sameShape), endpoint(catalog)] })).rejects.toThrow("owns no replica tables");
  await expect(connectDatabase(device, { open, sources: [endpoint(primary), { database: schemaIdentity(catalog), url: "http://public.example.com" }] })).rejects.toThrow("HTTPS");
  expect(opens).toBe(0);
});

test("a late auth promise cannot return queued calls from a retired account before contracts load", async () => {
  const primary = defineSchema({ tasks: defineTable({ title: v.string() }) });
  const device = defineSchema({ tasks: primary.tables.tasks.replica() });
  let resolve!: (value: { key: string }) => void;
  const auth = new Promise<{ key: string }>(value => { resolve = value; });
  const local = await initializeDatabase(device, { filename: ":memory:", sources: [{ database: schemaIdentity(primary), url: "https://api.example.com", auth }] });
  try {
    const pending = local.pendingMutations();
    await local.setAuth(schemaIdentity(primary), { key: "bob" });
    resolve({ key: "alice" });
    await expect(pending).rejects.toMatchObject({ code: "context_changed" });
    expect(await local.pendingMutations()).toEqual([]);
  } finally { resolve({ key: "alice" }); await local.close(); }
});
