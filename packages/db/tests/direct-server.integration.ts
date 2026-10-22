import { strict as assert } from "node:assert";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { pathToFileURL, fileURLToPath } from "node:url";
import { defineSchema, defineTable, sql, v, type TableReference } from "../src/schema.ts";
import type { Database, WriteDatabase } from "../src/server.ts";
import { runCommand } from "./integration-runtime.ts";

const url = process.env.HULLADB_TEST_URL!, namespace = process.env.HULLADB_TEST_NAMESPACE!;
if (!url || !namespace || !process.env.HULLADB_ENGINE) throw new Error("Run through the native direct_bridge integration target");
const documents = defineTable({ score: v.number(), settings: v.json({ theme: v.string().nullable().optional(), tags: v.array(v.string()), members: v.array(v.object({ name: v.string() })) }) });
const metrics = defineTable({ category: v.string().nullable(), amount: v.int64({ mode: "bigint" }).nullable(), score: v.number().nullable() });
class Amount { readonly cents: number; constructor(cents: number) { this.cents = cents; } }
const events = defineTable({ safe: v.int64(), at: v.timestamp(), precise: v.timestamp({ mode: 'string', precision: 6 }), local: v.timestamp({ withTimezone: false }), day: v.date({ mode: 'date' }), amount: v.number().codec({ encode: (value: Amount) => value.cents, decode: value => new Amount(value) }).default(new Amount(10)), label: v.string().codec({ encode: (value: bigint) => value.toString(), decode: value => BigInt(value) }), details: v.json({ tag: v.string().codec({ encode: (value: bigint) => value.toString(), decode: value => BigInt(value) }) }) });
const folders = defineTable({ slug: v.string() }, { primaryKey: 'slug' });
const links = defineTable({ folder: v.id(folders, { onDelete: 'cascade' }), label: v.string().min(1).max(30), low: v.int64().min(0).max(100), high: v.int64().min(0).max(100), state: v.string().oneOf(['draft', 'published']).default('draft') }, { checks: row => ({ valid_range: sql`${row.low} <= ${row.high}` }) }).index(['folder', 'state']).index(['label'], { name: 'by_label' });
const bookmarks = defineTable({ folder: v.id('Folders', { onDelete: 'setNull' }).nullable(), label: v.string() });
const schema = defineSchema({ Folders: folders, links, bookmarks, events, metrics, documents, tasks: defineTable({ title: v.string(), done: v.boolean().default(false) }, { unique: { title: { fields: ["title"] } } }) });
const root = await mkdtemp(new URL("./direct-project-", import.meta.url).pathname);
let db: Database<typeof schema> | undefined;
try {
  await mkdir(resolve(root, "db"));
  await writeFile(resolve(root, "db.config.ts"), `import { defineConfig } from '@hulla/db/config'; export default defineConfig(${JSON.stringify({ database: { server: { url, namespace } } })});`);
  await writeFile(resolve(root, "db/schema.ts"), `import { defineSchema, defineTable, sql, v } from '@hulla/db/schema';
export class Amount { readonly cents: number; constructor(cents: number) { this.cents = cents; } }
const folders = defineTable({ slug: v.string() }, { primaryKey: 'slug' });
const links = defineTable({ folder: v.id(folders, { onDelete: 'cascade' }), label: v.string().min(1).max(30), low: v.int64().min(0).max(100), high: v.int64().min(0).max(100), state: v.string().oneOf(['draft', 'published']).default('draft') }, { checks: row => ({ valid_range: sql\`\${row.low} <= \${row.high}\` }) }).index(['folder', 'state']).index(['label'], { name: 'by_label' });
const bookmarks = defineTable({ folder: v.id('Folders', { onDelete: 'setNull' }).nullable(), label: v.string() });
const events = defineTable({ safe: v.int64(), at: v.timestamp(), precise: v.timestamp({ mode: 'string', precision: 6 }), local: v.timestamp({ withTimezone: false }), day: v.date({ mode: 'date' }), amount: v.number().codec({ encode: (value: Amount) => value.cents, decode: value => new Amount(value) }).default(new Amount(10)), label: v.string().codec({ encode: (value: bigint) => value.toString(), decode: value => BigInt(value) }), details: v.json({ tag: v.string().codec({ encode: (value: bigint) => value.toString(), decode: value => BigInt(value) }) }) });
export default defineSchema({ Folders: folders, links, bookmarks, events, metrics: defineTable({ category: v.string().nullable(), amount: v.int64({ mode: "bigint" }).nullable(), score: v.number().nullable() }), documents: defineTable({ score: v.number(), settings: v.json({ theme: v.string().nullable().optional(), tags: v.array(v.string()), members: v.array(v.object({ name: v.string() })) }) }), tasks: defineTable({ title: v.string(), done: v.boolean().default(false) }, { unique: { title: { fields: ['title'] } } }) });`);
  async function command(name: string, succeeds = true) {
    const executable = name === "generate" ? process.env.BUN ?? "bun" : process.execPath;
    const { code, stdout, stderr } = await runCommand(executable, [fileURLToPath(new URL("../src/cli.ts", import.meta.url)), name, resolve(root, "db.config.ts")]);
    assert.equal(code === 0, succeeds, stderr);
    assert.equal((stdout + stderr).includes(url), false, "setup must not print the connection URL");
  }
  await command("generate");
  await command("init");
  await command("init"); // Explicit initialization is repeatable, never a reset.

  const generated = await import(pathToFileURL(resolve(root, "db/_generated/server.ts")).href);
  const { tasks } = await import(pathToFileURL(resolve(root, "db/_generated/tables.ts")).href) as { tasks: TableReference<typeof schema, "tasks"> };
  const database = db = generated.db as Database<typeof schema>;
  const folder = await database.insert('Folders', { slug: 'example' });
  const link = await database.insert('links', { folder: folder.slug, label: 'Guide', low: 10, high: 20 });
  const bookmark = await database.insert('bookmarks', { folder: folder.slug, label: 'Saved' });
  assert.equal(link.state, 'draft');
  assert.throws(() => database.insert('links', { folder: folder.slug, label: '', low: 0, high: 1 }));
  await assert.rejects(Promise.resolve(database.insert('links', { folder: folder.slug, label: 'Invalid', low: 20, high: 10 })), { code: 'invalid_input' });
  await assert.rejects(database.transaction(async tx => {
    await tx.insert('bookmarks', { folder: folder.slug, label: 'rollback' });
    await tx.update('links', current => ({ low: current.low.add(15) })).where({ id: link.id });
  }), { code: 'invalid_input' });
  assert.equal(await database.select('bookmarks').where({ label: 'rollback' }).unique(), null);
  assert.equal((await database.select('links').where({ id: link.id }).unique())!.low, 10);
  await database.delete('Folders').where({ slug: folder.slug });
  assert.equal(await database.select('links').where({ id: link.id }).unique(), null);
  assert.equal((await database.select('bookmarks').where({ id: bookmark.id }).unique())!.folder, null);
  const deferred = database.insert("tasks", { title: "one" });
  assert.deepEqual(await database.select("tasks"), []);
  const first = await deferred;
  assert.equal((await deferred).id, first.id);
  assert.equal((await database.select("tasks")).length, 1);
  assert.equal((await database.select("tasks").limit(100_000)).length, 1);
  assert.equal("row" in first, false);
  assert.equal("version" in first, false);

  await assert.rejects(database.transaction(async tx => {
    await tx.insert("tasks", { title: "rolled back" });
    throw new Error("ordinary callback failure");
  }), /ordinary callback failure/);
  assert.equal(await database.select("tasks").where({ title: "rolled back" }).unique(), null);

  await assert.rejects(database.transaction(async tx => {
    await tx.insert("tasks", { title: "must rollback" });
    try { await tx.insert("tasks", { title: "one" }); } catch { /* Cannot revive a failed SQL transaction. */ }
  }), { code: "constraint_violation" });
  assert.equal(await database.select("tasks").where({ title: "must rollback" }).unique(), null);

  let escaped!: WriteDatabase<typeof schema>;
  let entered!: () => void, release!: () => void;
  const inside = new Promise<void>(resolve => { entered = resolve; });
  const barrier = new Promise<void>(resolve => { release = resolve; });
  const transaction = database.transaction(async tx => {
    escaped = tx;
    await assert.rejects(Promise.resolve(database.select("tasks")), { code: "transaction_scope" });
    entered();
    await barrier;
    await tx.update("tasks", { done: true }).where({ id: first.id });
  });
  await inside;
  await assert.rejects(Promise.resolve(escaped.select("tasks")), { code: "transaction_scope" });
  // Another request outside the callback's async scope remains independent.
  assert.equal((await database.select("tasks")).length, 1);
  release();
  await transaction;
  await assert.rejects(Promise.resolve(escaped.select("tasks")), { code: "transaction_closed" });
  assert.equal((await database.select("tasks").where({ id: first.id }).unique())!.done, true);

  let attempts = 0;
  await assert.rejects(database.transaction(async () => { attempts++; throw new Error("do not retry"); }, { retries: 3 }), /do not retry/);
  assert.equal(attempts, 1);

  // Both callbacks read the same snapshot before either writes. Exactly one
  // must retry after confirmed serialization rollback, then read the new row.
  let readers = 0, bothRead!: () => void;
  const readBarrier = new Promise<void>(resolve => { bothRead = resolve; });
  const runs: [number, number] = [0, 0];
  await Promise.all(runs.map((_, index) => database.transaction(async tx => {
    runs[index] = (runs[index] ?? 0) + 1;
    const task = await tx.select("tasks").where({ id: first.id }).unique();
    assert.ok(task);
    if (runs[index] === 1) {
      if (++readers === 2) bothRead();
      await readBarrier;
    }
    await tx.update("tasks", { done: !task.done }).where({ id: first.id });
  }, { retries: 2 })));
  assert.equal(runs[0] + runs[1], 3);
  assert.equal((await database.select("tasks").where({ id: first.id }).unique())!.done, true);
  await assert.rejects(database.transaction(async tx => {
    void tx.insert("tasks", { title: "unawaited" }).then(() => {}, () => {});
  }), { code: "unawaited_operations" });
  assert.equal(await database.select("tasks").where({ title: "unawaited" }).unique(), null);

  const inserted = await database.insert("tasks", [{ title: "two" }, { title: "three" }]);
  assert.equal(inserted.length, 2);
  const firstPage = await database.select(tasks).orderBy(tasks.title).paginate({ limit: 2 });
  assert.deepEqual(firstPage.page.map(row => row.title), ["one", "three"]);
  assert.ok(firstPage.cursor);
  const lastPage = await database.select(tasks).orderBy(tasks.title).paginate({ limit: 2, cursor: firstPage.cursor });
  assert.deepEqual(lastPage.page.map(row => row.title), ["two"]);
  assert.equal(lastPage.cursor, null);
  await assert.rejects(Promise.resolve(database.select(tasks).where({ done: true }).orderBy(tasks.title)
    .paginate({ limit: 2, cursor: firstPage.cursor })), { code: "invalid_input" });
  assert.deepEqual((await database.select(tasks)
    .where(({ or, eq, startsWith }) => or(eq(tasks.done, true), startsWith("title", "t")))
    .orderBy(tasks.done).orderBy("title", "desc")).map(row => row.title), ["two", "three", "one"]);
  assert.deepEqual((await database.select("tasks").where(tasks.title, "contains", "TH", { caseSensitive: false })).map(row => row.title), ["three"]);
  assert.equal((await database.delete(tasks).where(({ and, not, eq }) => and(not(eq(tasks.done, true)), eq("title", "two")))).length, 1);
  assert.equal((await database.update("tasks", { done: true }).where(true)).length, 2);
  // Projections omit fields across the native bridge while filters and cursor
  // ordering still use the complete database row.
  for (const includeDone of [false, true]) {
    const projected = await database.select(tasks, { title: true, done: includeDone })
      .where('done', '=', true).orderBy('title');
    assert.deepEqual(projected, [
      { title: 'one', ...(includeDone ? { done: true } : {}) },
      { title: 'three', ...(includeDone ? { done: true } : {}) },
    ]);
  }
  const narrowPage = await database.select(tasks, ['title']).orderBy('done').orderBy('title').paginate({ limit: 1 });
  assert.deepEqual(narrowPage.page, [{ title: 'one' }]);
  assert.ok(narrowPage.cursor);
  const nextNarrowPage = await database.select(tasks, ['title']).orderBy('done').orderBy('title')
    .paginate({ limit: 1, cursor: narrowPage.cursor });
  assert.deepEqual(nextNarrowPage.page, [{ title: 'three' }]);
  const scratch = await database.insert(tasks, { title: 'projection scratch' }).returning(['id']);
  assert.deepEqual(Object.keys(scratch), ['id']);
  assert.deepEqual(await database.update(tasks, { done: true }).where({ id: scratch.id }).returning({ title: true }), [{ title: 'projection scratch' }]);
  assert.deepEqual(await database.delete(tasks).where({ id: scratch.id }).returning(['done']), [{ done: true }]);
  assert.deepEqual(await database.insert(tasks, []).returning(['title']), []);
  await assert.rejects(database.transaction(async tx => {
    await tx.insert(tasks, { title: 'projected rollback' }).returning(['id']);
    await tx.insert(tasks, { title: 'one' }).returning(['id']);
  }));
  assert.equal(await database.select(tasks, ['title']).where({ title: 'projected rollback' }).unique(), null);
  // Exercise JSON across the generated bridge and real PostgreSQL, not only TS validation.
  await database.insert("documents", [
    { score: 1.5, settings: { tags: [], members: [] } },
    { score: 2.25, settings: { theme: null, tags: ["blue"], members: [{ name: "Ada" }] } },
    { score: 3.75, settings: { theme: "dark", tags: ["blue", "green"], members: [] } },
  ]);
  assert.equal((await database.select("documents").where("settings.theme", "=", null)).length, 2);
  assert.equal((await database.select("documents").where("settings.theme", "exists")).length, 2);
  assert.equal((await database.select("documents").where("settings.members", "contains", { name: "Ada" })).length, 1);
  assert.equal((await database.select("documents").where(({ gte, arrayLength }) => gte(arrayLength("settings.tags"), 2))).length, 1);
  assert.deepEqual((await database.select("documents").orderBy("score").paginate({ limit: 2 })).page.map(row => row.score), [1.5, 2.25]);
  await database.update("documents", { settings: { tags: [], members: [] } }).where("score", "=", 3.75);
  assert.equal((await database.select("documents").where("settings.theme", "not exists")).length, 2);
  await assert.rejects(database.transaction(async tx => {
    await tx.insert("documents", { score: 9, settings: { tags: [], members: [] } });
    await tx.insert("documents", { score: 10, settings: { tags: [], members: [], unexpected: true } } as never);
  }));
  assert.equal((await database.select("documents").where("score", ">=", 9)).length, 0);
  await database.insert('metrics', [
    { category: 'a', amount: 1n, score: 1.5 },
    { category: 'a', amount: 2n, score: null },
    { category: 'b', amount: 10n, score: 4 },
    { category: 'b', amount: 20n, score: 6 },
    { category: 'empty', amount: null, score: null },
    { category: 'wide', amount: 9223372036854775807n, score: null },
    { category: 'wide', amount: 9223372036854775807n, score: null },
  ]);
  assert.throws(() => database.insert('metrics', { category: 'invalid', amount: 9223372036854775808n, score: null }));
  const summary = await database.select('metrics').aggregate(a => ({ count: a.count(), total: a.sum('amount'), score: a.sum('score'), minimum: a.min('amount'), maximum: a.max('amount') }));
  assert.deepEqual(summary, { count: 7, total: 18446744073709551647n, score: 11.5, minimum: 1n, maximum: 9223372036854775807n });
  const empty = await database.select('metrics').where('category', '=', 'absent').aggregate(a => ({ count: a.count(), sum: a.sum('amount'), avg: a.avg('score'), min: a.min('category'), max: a.max('score') }));
  assert.deepEqual(empty, { count: 0, sum: null, avg: null, min: null, max: null });
  const allNull = await database.select('metrics').where('category', '=', 'empty').aggregate(a => ({ count: a.count(), sum: a.sum('amount') }));
  assert.deepEqual(allNull, { count: 1, sum: null });
  const grouped = await database.select('metrics').where('category', 'in', ['a', 'b'])
    .groupBy('category').aggregate(a => ({ total: a.sum('amount'), mean: a.avg('amount') }))
    .having(({ gt }) => gt('mean', '2')).orderBy('total', 'desc').limit(1);
  assert.equal(grouped.length, 1);
  assert.equal(grouped[0]!.category, 'b');
  assert.equal(grouped[0]!.total, 30n);
  assert.match(grouped[0]!.mean!, /^15(?:\.0+)?$/);
  const wide = await database.select('metrics').groupBy('category').aggregate(a => ({ total: a.sum('amount') }))
    .having(({ gt }) => gt('total', 9223372036854775807n));
  assert.deepEqual(wide, [{ category: 'wide', total: 18446744073709551614n }]);
  await assert.rejects(database.transaction(async tx => {
    await tx.insert('metrics', { category: 'rollback', amount: 7n, score: 7 });
    assert.deepEqual(await tx.select('metrics').where('category', '=', 'rollback').aggregate(a => ({ total: a.sum('amount') })), { total: 7n });
    throw new Error('aggregate snapshot rollback');
  }), /aggregate snapshot rollback/);
  assert.deepEqual(await database.select('metrics').where('category', '=', 'rollback').aggregate(a => ({ count: a.count() })), { count: 0 });
  // Cardinality guards must run before the root operation commits, and a caught
  // guard failure must also abort earlier writes in an explicit transaction.
  await assert.rejects(Promise.resolve(database.insert('tasks', { title: 'count rollback' }).returning(['id']).expectRows(2)), { code: 'row_expectation_failed' });
  assert.equal(await database.select('tasks').where({ title: 'count rollback' }).unique(), null);
  await assert.rejects(database.transaction(async tx => {
    await tx.insert('tasks', { title: 'before count failure' });
    try { await tx.update('tasks', { done: true }).where({ title: 'absent' }).expectRows(n => n > 0); } catch {}
    return 'caught';
  }), { code: 'row_expectation_failed' });
  assert.equal(await database.select('tasks').where({ title: 'before count failure' }).unique(), null);
  await assert.rejects(database.transaction(async tx => {
    try { await tx.insert('tasks', { title: 'undefined failure' }).expectRows(() => { throw undefined; }); } catch {}
  }), error => error === undefined);
  assert.equal(await database.select('tasks').where({ title: 'undefined failure' }).unique(), null);
  await assert.rejects(database.transaction(async tx => {
    await tx.insert('tasks', { title: 'before async predicate' });
    try { await tx.insert('tasks', { title: 'async predicate' }).expectRows((async () => true) as never); } catch {}
  }), TypeError);
  assert.deepEqual(await database.select('tasks').where('title', 'in', ['before async predicate', 'async predicate']), []);
  await assert.rejects(Promise.resolve(database.insert('tasks', []).expectRows(1)), { code: 'row_expectation_failed' });
  assert.deepEqual(await database.update('tasks', { done: true }).where({ title: 'one' }).expectRows(1).returning(['done']), [{ done: true }]);
  await assert.rejects(Promise.resolve(database.delete('tasks').where({ title: 'one' }).expectRows(2)), { code: 'row_expectation_failed' });
  assert.ok(await database.select('tasks').where({ title: 'one' }).unique());
  const skip = database.insert('tasks', { title: 'one', done: false })
    .onConflict(({ on }) => on('title').skip()).returning(['id']).expectRows(0);
  assert.equal(await skip, null);
  assert.equal(await skip, null);
  const conflictWrite = await database.insert('tasks', { title: 'one', done: false })
    .onConflict(({ on }) => on('title').update(({ incoming }) => ({ done: incoming.done })))
    .returning(['id', 'done']).expectRows(1);
  assert.deepEqual(conflictWrite, { id: first.id, done: false });
  const bulkSkip = await database.insert('tasks', [{ title: 'one' }, { title: 'conflict new' }])
    .onConflict(({ on }) => on('title').skip()).returning(['title']).expectRows(1);
  assert.deepEqual(bulkSkip, [{ title: 'conflict new' }]);
  await database.delete('tasks').where({ title: 'conflict new' }).expectRows(n => n === 1);
  const counter = await database.insert('metrics', { category: 'counter', amount: 2n, score: null });
  const bumped = await database.insert('metrics', { id: counter.id, category: 'unused', amount: 3n, score: null })
    .onConflict(({ on }) => on('id').update(({ current, incoming }) => ({ amount: current.amount.add(incoming.amount) })))
    .returning(['amount']).expectRows(1);
  assert.deepEqual(bumped, { amount: 5n });
  const incremented = await database.update('metrics', current => ({ amount: current.amount.add(1n) }))
    .where({ id: counter.id }).returning(['amount']).expectRows(1);
  assert.deepEqual(incremented, [{ amount: 6n }]);
  const instant = new Date('2026-10-20T12:34:56.789Z');
  const eventInput = { safe: 42, at: instant, precise: '2026-10-20T12:34:56.123456Z', local: '2026-10-20T08:00:00.123', day: new Date('2026-10-20T17:00:00.000Z'), label: 100n, details: { tag: 7n } };
  const event = await database.insert('events', eventInput);
  assert.equal(event.safe, 42);
  assert.equal(event.at.toISOString(), instant.toISOString());
  assert.equal(event.day.toISOString(), '2026-10-20T00:00:00.000Z');
  assert.equal(event.precise, '2026-10-20T12:34:56.123456Z');
  assert.equal(event.local, '2026-10-20T08:00:00.123');
  assert.equal(event.amount.cents, 10);
  assert.equal(event.label, 100n);
  assert.equal(event.details.tag, 7n);
  const selected = await database.select('events').where('label', '=', 100n).where('label', 'contains', '00').where('at', '>=', instant).unique();
  assert.ok(selected);
  assert.equal(selected.amount.cents, 10);
  assert.deepEqual(await database.select('events').aggregate(a => ({ total: a.sum('safe'), amount: a.sum('amount') })), { total: 42, amount: 10 });
  await database.update('events', { amount: new Amount(20), details: { tag: 9n } }).where({ id: event.id });
  assert.deepEqual({ ...(await database.select('events').where({ id: event.id }).unique())!.details }, { tag: 9n });
  assert.throws(() => database.insert('events', { ...eventInput, safe: Number.MAX_SAFE_INTEGER + 1 }));
  await assert.rejects(database.transaction(async tx => {
    await tx.insert('events', eventInput);
    throw new Error('codec rollback');
  }), /codec rollback/);
  assert.equal((await database.select('events')).length, 1);
  await writeFile(resolve(root, "db/schema.ts"), `import { defineSchema, defineTable, v } from '@hulla/db/schema'; export default defineSchema({ tasks: defineTable({ different: v.string() }) });`);
  await command("init", false);
  assert.equal((await database.select("tasks")).length, 2, "incompatible initialization must not reset existing data");
  await database.close();
  await assert.rejects(Promise.resolve(database.select("tasks")), { code: "database_closed" });
  console.log("PASS: generated direct server database, transactions, scope isolation, rollback and lazy builders");
} finally {
  await db?.close();
  await rm(root, { recursive: true, force: true });
}
