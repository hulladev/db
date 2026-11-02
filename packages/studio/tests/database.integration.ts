import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { createDatabase } from "@hulla/db/server";
import { runCommand } from "../../db/tests/integration-runtime.ts";
import { startStudio, type StudioPage, type StudioRow } from "../src/index.ts";
import schema from "./fixtures/schema.ts";

const url = process.env.HULLADB_TEST_URL!, namespace = process.env.HULLADB_TEST_NAMESPACE!;
if (!url || !namespace || !process.env.HULLADB_ENGINE) throw new Error("Run through the native direct_bridge Studio integration target");
const root = await mkdtemp(fileURLToPath(new URL("./studio-project-", import.meta.url)));
const connection = { url, namespace }, database = createDatabase(schema, connection);
let studio: Awaited<ReturnType<typeof startStudio>> | undefined;
try {
  const config = `${root}/db.config.ts`;
  await writeFile(config, `export default ${JSON.stringify({ database: { server: connection }, schema: fileURLToPath(new URL("./fixtures/schema.ts", import.meta.url)) })};`);
  const initialized = await runCommand(process.execPath, [fileURLToPath(new URL("../../db/src/cli.ts", import.meta.url)), "init", config]);
  assert.equal(initialized.code, 0, initialized.stderr);
  await database.insert("folders", { slug: "work" });
  const entries = await database.insert("entries", Array.from({ length: 5 }, (_, index) => ({
    folder: "work", title: `Entry ${index}`, amount: 9223372036854775807n - BigInt(index),
    day: index === 0 ? new Date("2026-10-26T00:00:00Z") : null,
    details: { tags: ["initial"] }, label: BigInt(index),
  })));
  studio = await startStudio({ schema, connection, port: 0 });
  const address = new URL(studio.url), token = new URLSearchParams(address.hash.slice(1)).get("token");
  const post = (path: string, body: unknown) => fetch(`${address.origin}/api/${path}`, {
    method: "POST", headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" }, body: JSON.stringify(body),
  });
  async function page(body: Record<string, unknown> = {}): Promise<StudioPage> {
    const response = await post("rows", { table: "entries", sort: { field: "title", direction: "asc" }, ...body });
    assert.equal(response.status, 200, await response.clone().text()); return response.json();
  }
  const first = await page({ limit: 2 });
  assert.deepEqual(first.rows.map(row => row.values.title), ["Entry 0", "Entry 1"]);
  assert.equal(first.rows[0]!.values.amount, "9223372036854775807");
  assert.equal(first.rows[0]!.values.day, "2026-10-26");
  assert.equal(first.rows[0]!.values.label, "0");
  const second = await page({ limit: 2, cursor: first.cursor });
  assert.deepEqual(second.rows.map(row => row.values.title), ["Entry 2", "Entry 3"]);
  assert.equal((await page({ filters: [{ field: "title", operator: "contains", value: "2" }] })).rows.length, 1);
  const target = first.rows[0]!;
  const update = (row: StudioRow, patch: unknown) => post("update", { table: "entries", key: row.values.id, snapshot: row.snapshot, patch });
  const edited = await update(target, { title: "Studio edit", amount: "-9223372036854775808", day: null, details: { tags: ["edited"] }, label: "99" });
  assert.equal(edited.status, 200, await edited.clone().text());
  const current = await database.select("entries").where({ id: entries[0]!.id }).unique();
  assert.equal(current!.title, "Studio edit");
  assert.equal(current!.amount, -(1n << 63n));
  assert.equal(current!.day, null);
  assert.equal(current!.label, 99n);
  assert.deepEqual(current!.details.tags, ["edited"]);
  assert.equal((await update(target, { title: "Lost update" })).status, 409);
  const refreshed = await edited.json() as StudioRow;
  await database.update("entries", { done: true }).where({ id: entries[0]!.id });
  assert.equal((await update(refreshed, { title: "Overwrite concurrent change" })).status, 409);
  const latest = (await page({ filters: [{ field: "id", operator: "=", value: entries[0]!.id }] })).rows[0]!;
  assert.equal((await update(latest, { id: entries[1]!.id })).status, 400);
  assert.equal((await update(latest, { title: "" })).status, 400);
  assert.equal((await update(latest, { title: "Entry 1" })).status, 422);
  assert.equal((await update({ ...latest, snapshot: "0".repeat(64) }, { title: "Forged" })).status, 409);
  await database.delete("entries").where({ id: entries[0]!.id });
  assert.equal((await update(latest, { title: "Resurrect deleted row" })).status, 409);
  assert.equal(await database.select("entries").where({ id: entries[0]!.id }).unique(), null);
  const remove = (table: string, selected: readonly StudioRow[], primaryKey = "id") => post("delete", { table, rows: selected.map(row => ({ key: row.values[primaryKey], snapshot: row.snapshot })) });
  const before = (await page()).rows;
  assert.equal((await post("delete", { table: "entries", rows: [] })).status, 400);
  assert.equal((await remove("entries", [before[0]!, before[0]!])).status, 400);
  assert.equal((await remove("entries", Array(201).fill(before[0]!))).status, 400);
  await database.update("entries", { done: true }).where({ id: entries[1]!.id });
  assert.equal((await remove("entries", before.slice(0, 2))).status, 409);
  assert.equal((await remove("entries", [latest, before[1]!])).status, 409);
  const fresh = (await page()).rows;
  assert.equal((await remove("entries", [{ ...fresh[0]!, snapshot: "0".repeat(64) }, fresh[1]!])).status, 409);
  assert.equal((await database.select("entries")).length, 4, "a stale/missing/forged selection must never partially delete rows");
  await database.insert("folders", { slug: "free" });
  const folders = (await page({ table: "folders", sort: { field: "slug", direction: "asc" } })).rows;
  assert.equal((await remove("folders", folders, "slug")).status, 422);
  assert.deepEqual((await database.select("folders").orderBy("slug")).map(row => row.slug), ["free", "work"], "a restricted reference rolls back the entire batch, including the unreferenced folder");
  await database.insert("attachments", [{ entry: entries[1]!.id, body: "first" }, { entry: entries[2]!.id, body: "second" }]);
  const deleted = await remove("entries", fresh.slice(0, 2));
  assert.equal(deleted.status, 200, await deleted.clone().text());
  assert.equal((await deleted.json()).deleted, 2);
  assert.equal((await database.select("entries")).length, 2);
  assert.equal((await database.select("attachments")).length, 0, "managed deletion must apply cascade rules");
  await database.insert("counters", [{ id: 9007199254740992n, title: "first" }, { id: 9007199254740993n, title: "second" }]);
  const counters = (await page({ table: "counters", sort: { field: "id", direction: "asc" } })).rows;
  assert.equal((await remove("counters", [counters[0]!])).status, 200);
  assert.deepEqual((await database.select("counters")).map(row => row.id), [9007199254740993n], "lossless primary keys must select exactly the intended row");
  console.log("Studio managed editing, bulk deletion, codecs, pagination and stale-row checks passed");
} finally { await studio?.close(); await database.close(); await rm(root, { recursive: true, force: true }); }
