import { defineSchema, defineTable, v, createTables, type Row } from "../src/schema.ts";
import type { WriteDatabase } from "../src/database.ts";
class Money { constructor(readonly cents: number) {} }
const schema = defineSchema({ items: defineTable({
  amount: v.number().codec({ encode: (value: Money) => value.cents, decode: value => new Money(value) }),
  link: v.string().codec({ encode: (value: URL) => value.href, decode: value => new URL(value) }),
  raw: v.number(), text: v.string(),
  number: v.int64(), bigint: v.int64({ mode: "bigint" }),
  at: v.timestamp(), local: v.timestamp({ withTimezone: false }), date: v.date(),
  precise: v.timestamp({ mode: "string", precision: 6 }),
}) });
const tables = createTables<typeof schema>({ items: {} });
declare const db: WriteDatabase<typeof schema>;
declare const row: Row<typeof schema, "items">;
const money: Money = row.amount;
const url: URL = row.link;
const number: number = row.number;
const bigint: bigint = row.bigint;
const instant: Date = row.at;
const calendar: string = row.date;
const local: string = row.local;
const precise: string = row.precise;
void [money, url, number, bigint, instant, calendar, local, precise];
db.select("items").where("amount", ">=", new Money(12)).where("link", "contains", "example.com", { caseSensitive: false });
db.select("items").where(({ gte, contains, and }) => and(gte(tables.items.amount, new Money(12)), contains(tables.items.link, "example.com")));
db.select("items").where("at", ">", new Date());
db.select("items").where(({ gt }) => gt(tables.items.at, new Date()));
db.update("items", current => ({ amount: current.amount.add(1), raw: current.amount, text: current.link, number: current.bigint, bigint: current.number })).where(true);
const sum = db.select("items").aggregate(a => ({ sum: a.sum("amount"), min: a.min(tables.items.amount), average: a.avg("number"), instant: a.min("at") }));
sum.then(result => { const values: [number | null, number | null, number | null, Date | null] = [result.sum, result.min, result.average, result.instant]; void values; });
db.select("items").groupBy("amount").aggregate(a => ({ n: a.count() })).having(({ gt }) => gt("amount", new Money(10)));
// @ts-expect-error Default Date representation cannot preserve microseconds.
v.timestamp({ precision: 6 });
// @ts-expect-error Explicit Date representation cannot preserve microseconds.
v.timestamp({ withTimezone: false, mode: "date", precision: 4 });
// @ts-expect-error Whole-value comparisons accept the decoded application type.
db.select("items").where("amount", ">=", 12);
// @ts-expect-error Numeric storage has no text matching operations.
db.select("items").where("amount", "contains", "12");
// @ts-expect-error Numeric storage has no text matching operations.
db.select("items").where(({ contains }) => contains("amount", "12"));
// @ts-expect-error SQL arithmetic uses the underlying numeric representation.
db.update("items", current => ({ amount: current.amount.add(new Money(1)) }));
// @ts-expect-error Text storage cannot be summed even if its codec returns a number.
db.select("items").aggregate(a => ({ sum: a.sum("link") }));
