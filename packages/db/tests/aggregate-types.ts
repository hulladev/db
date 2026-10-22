import type { ReadDatabase } from "../src/database.ts";
import { defineSchema, defineTable, v } from "../src/schema.ts";
import { createTables } from "../src/fields.ts";

const schema = defineSchema({
  orders: defineTable({ owner: v.string(), paid: v.boolean(), amount: v.int64({ mode: "bigint" }).nullable(), score: v.number(), settings: v.json({ theme: v.string() }) }),
});
declare const db: ReadDatabase<typeof schema>;
const { orders } = createTables<typeof schema>({ orders: { id: {}, owner: {}, paid: {}, amount: {}, score: {}, settings: { jsonShape: { kind: "object", fields: { theme: { shape: { kind: "string" }, optional: false } } } } } });
const totals = db.select(orders).aggregate(a => ({
  count: a.count(), sum: a.sum("amount"), average: a.avg(orders.amount),
  score: a.avg("score"), low: a.min("owner"), high: a.max(orders.score),
}));
const grouped = db.select("orders").where("paid", "=", true).groupBy("owner", "paid")
  .aggregate(a => ({ total: a.count(), amount: a.sum("amount"), average: a.avg("amount") }))
  .having(({ and, gte, startsWith }) => and(gte("total", 2), gte("average", "2.25"), startsWith("owner", "a")))
  .orderBy("amount", "desc").limit(5);

type Assert<T extends true> = T;
type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends (<T>() => T extends B ? 1 : 2) ? true : false;
type Normalize<T> = { [K in keyof T]: T[K] };
export type Summary = Assert<Equal<Awaited<typeof totals>, { count: number; sum: bigint | null; average: string | null; score: number | null; low: string | null; high: number | null }>>;
export type Group = Assert<Equal<Normalize<Awaited<typeof grouped>[number]>, { owner: string; paid: boolean; total: number; amount: bigint | null; average: string | null }>>;
declare const numericField: "amount" | "score";
const dynamic = db.select("orders").aggregate(a => ({ average: a.avg(numericField) }));
export type DynamicAverage = Assert<Equal<Awaited<typeof dynamic>, { average: string | number | null }>>;
// @ts-expect-error aggregates require columns appropriate to their operation
db.select("orders").aggregate(a => ({ total: a.sum("owner") }));
// @ts-expect-error grouping JSON would need a separate representation contract
db.select("orders").groupBy("settings");
// @ts-expect-error grouped results cannot overwrite their group keys
db.select("orders").groupBy("owner").aggregate(a => ({ owner: a.count() }));
// @ts-expect-error JSON paths cannot be grouped
db.select("orders").groupBy("settings.theme");
// @ts-expect-error grouping must name at least one field
db.select("orders").groupBy();
// @ts-expect-error decimal aggregate strings have numeric semantics, not text matching
db.select("orders").groupBy("owner").aggregate(a => ({ average: a.avg("amount") })).having(({ startsWith }) => startsWith("average", "2"));
// @ts-expect-error ordinary input columns are not grouped output columns
db.select("orders").groupBy("owner").aggregate(a => ({ total: a.count() })).orderBy("amount");
// @ts-expect-error grouped cursor pagination is not supported
db.select("orders").groupBy("owner").aggregate(a => ({ total: a.count() })).paginate({ limit: 10 });
// @ts-expect-error count counts matching rows, not a supplied field
db.select("orders").aggregate(a => ({ total: a.count("amount") }));
