import { expect, test } from "bun:test";
import { compileSchema, defineSchema, defineTable, v } from "../src/schema.ts";
import { schemaDescriptor } from "../src/schema-contract.ts";
import { transactionDatabase, type WriteDatabase } from "../src/database.ts";
import { createTables } from "../src/fields.ts";

class Money { constructor(readonly cents: number) {} }
const money = v.number().codec({ encode: (value: Money) => value.cents, decode: value => new Money(value) });
const url = v.string().codec({ encode: (value: URL) => value.href, decode: value => new URL(value) });
const id = "00000000-0000-0000-0000-000000000001";

test("field codecs encode whole-value predicates and writes while SQL expressions and text patterns retain storage semantics", async () => {
  const schema = defineSchema({ items: defineTable({ amount: money, link: url }) });
  const tables = createTables<typeof schema>(Object.fromEntries(compileSchema(schema).map(table => [table.name, table.fields])));
  const requests: Record<string, unknown>[] = [];
  const db = transactionDatabase(schema, true, async operation => {
    requests.push(operation);
    if (operation.method === "aggregate") return { total: 450, minimum: 100 };
    const row = { row: { id, amount: 250, link: "https://example.com/" }, version: "1" };
    return operation.method === "insert" ? row : [row];
  }) as WriteDatabase<typeof schema>;
  const written = await db.insert("items", { amount: new Money(250), link: new URL("https://example.com/") });
  expect(written.amount).toBeInstanceOf(Money);
  expect(written.amount.cents).toBe(250);
  expect(written.link).toBeInstanceOf(URL);
  expect(requests[0]?.values).toEqual({ amount: 250, link: "https://example.com/" });
  await db.select("items").where("amount", ">=", new Money(100)).where("link", "contains", "example.com");
  expect(requests[1]?.filters).toEqual([
    { kind: "compare", field: "amount", operator: ">=", operand: { kind: "value", value: 100 } },
    { kind: "compare", field: "link", operator: "contains", operand: { kind: "value", value: "example.com" }, caseSensitive: true },
  ]);
  await db.select(tables.items).where(({ eq, contains }) => {
    contains(tables.items.link, "example.com");
    return eq("link", new URL("https://example.com/"));
  });
  expect(requests[2]?.filters).toEqual([{ kind: "compare", field: "link", operator: "=", operand: { kind: "value", value: "https://example.com/" } }]);
  await db.update("items", current => ({ amount: current.amount.add(25) })).where(true);
  expect(requests[3]?.values).toEqual({ amount: { kind: "add", left: { kind: "field", source: "current", field: "amount" }, right: { kind: "value", value: 25 } } });
  expect(await db.select("items").aggregate(a => ({ total: a.sum("amount"), minimum: a.min(tables.items.amount) }))).toEqual({ total: 450, minimum: 100 });
  expect(() => db.insert("items", { amount: new Money(Infinity), link: new URL("https://example.com/") })).toThrow("float64");
});

test("nullable and omitted fields bypass callbacks and encoded defaults stay in the database contract", async () => {
  let encodes = 0;
  let decodes = 0;
  const field = v.string().codec({ encode: (value: URL) => { encodes++; return value.href; }, decode: value => { decodes++; return new URL(value); } }).nullable().default(null);
  const schema = defineSchema({ links: defineTable({ link: field }) });
  encodes = 0; decodes = 0;
  const requests: Record<string, unknown>[] = [];
  const db = transactionDatabase(schema, true, async operation => { requests.push(operation); return { row: { id, link: null }, version: "1" }; }) as WriteDatabase<typeof schema>;
  expect((await db.insert("links", {})).link).toBeNull();
  expect(requests[0]?.values).toEqual({});
  expect(encodes).toBe(0); expect(decodes).toBe(0);
  const defaultSchema = defineSchema({ prices: defineTable({ amount: money.default(new Money(250)) }) });
  expect((schemaDescriptor(defaultSchema) as { tables: { fields: { name: string; default: unknown }[] }[] }).tables[0]!.fields.find(field => field.name === "amount")!.default).toEqual(["float64", 250]);
  expect(() => v.string().codec({ encode: (() => Promise.resolve("x")) as never, decode: value => value }).parse("x")).toThrow("synchronous");
});

test("JSON conversions require explicit codecs and preserve strict shapes without invoking accessors", async () => {
  const field = v.json({ count: v.string().codec({ encode: (value: bigint) => value.toString(), decode: value => BigInt(value) }), history: v.array(v.object({ url })) });
  const app = { count: 12n, history: [{ url: new URL("https://example.com/") }] };
  const encoded = field.encode(app);
  expect(encoded).toEqual({ count: "12", history: [{ url: "https://example.com/" }] });
  const decoded = field.decode(encoded);
  expect(decoded.count).toBe(12n);
  expect(decoded.history[0]!.url).toBeInstanceOf(URL);
  expect(() => v.json({ when: v.timestamp() })).toThrow("explicit");
  expect(() => v.json().encode(app)).toThrow("JSON");
  let invoked = false;
  expect(() => field.encode({ get count() { invoked = true; return 12n; }, history: [] })).toThrow("accessors");
  expect(invoked).toBe(false);
  const schema = defineSchema({ events: defineTable({ detail: field, numbers: v.json(v.array(v.string().codec({ encode: (value: bigint) => String(value), decode: value => BigInt(value) }))) }) });
  let filters: unknown;
  const db = transactionDatabase(schema, false, async operation => { filters = operation.filters; return []; }) as WriteDatabase<typeof schema>;
  await db.select("events").where("numbers", "contains", 12n).where("detail.count", "=", 12n);
  expect(filters).toEqual([
    { kind: "compare", field: "numbers", operator: "contains", operand: { kind: "value", value: "12" } },
    { kind: "compare", field: "detail.count", operator: "=", operand: { kind: "value", value: "12" } },
  ]);
});

test("integer number mode rejects unsafe reads/writes/sums while bigint mode preserves wide sums", async () => {
  const number = v.int64();
  expect(number.encode(12)).toBe(12n);
  expect(number.decode("12")).toBe(12);
  expect(() => number.encode(Number.MAX_SAFE_INTEGER + 1)).toThrow("safe integer");
  expect(() => number.decode("9223372036854775807")).toThrow("safe integer");
  const schema = defineSchema({ values: defineTable({ n: number, b: v.int64({ mode: "bigint" }), at: v.timestamp(), customAt: v.timestamp().codec({ encode: (value: number) => new Date(value), decode: value => value.getTime() }) }) });
  const output = [{ total: "9007199254740992" }, { total: "18446744073709551614" }, { average: "1.5000000000000000" }, { earliest: "2024-02-29T12:30:00Z", rawEarliest: "2024-02-29T12:30:00Z" }];
  const db = transactionDatabase(schema, false, async () => output.shift()) as WriteDatabase<typeof schema>;
  await expect(Promise.resolve(db.select("values").aggregate(a => ({ total: a.sum("n") })))).rejects.toThrow("safe integer");
  expect(await db.select("values").aggregate(a => ({ total: a.sum("b") }))).toEqual({ total: 18446744073709551614n });
  expect(await db.select("values").aggregate(a => ({ average: a.avg("n") }))).toEqual({ average: 1.5 });
  expect(await db.select("values").aggregate(a => ({ earliest: a.min("at"), rawEarliest: a.min("customAt") }))).toEqual({ earliest: new Date("2024-02-29T12:30:00Z"), rawEarliest: "2024-02-29T12:30:00.000Z" });
});

test("calendar dates and timestamp modes normalize UTC without silently losing precision", () => {
  const date = v.date();
  expect(date.parse("2024-02-29")).toBe("2024-02-29");
  expect(() => date.parse("2023-02-29")).toThrow("calendar");
  expect(v.date({ mode: "date" }).encode(new Date("2024-02-29T23:59:59Z"))).toBe("2024-02-29");
  expect(v.date({ mode: "date" }).decode("2024-02-29").toISOString()).toBe("2024-02-29T00:00:00.000Z");
  const instant = v.timestamp();
  expect(instant.encode(new Date("2024-02-29T12:30:00.123+02:00"))).toBe("2024-02-29T10:30:00.123Z");
  expect(instant.decode("2024-02-29T10:30:00.123Z")).toEqual(new Date("2024-02-29T10:30:00.123Z"));
  expect(v.timestamp({ mode: "string", precision: 6 }).parse("2024-02-29T12:30:00.123456+02:00")).toBe("2024-02-29T10:30:00.123456Z");
  expect(v.timestamp({ withTimezone: false }).parse("2024-02-29T12:30:00")).toBe("2024-02-29T12:30:00.000");
  expect(v.timestamp({ withTimezone: false, mode: "date" }).decode("2024-02-29T12:30:00")).toEqual(new Date("2024-02-29T12:30:00Z"));
  expect(() => v.timestamp({ precision: 6 } as never)).toThrow("Date mode");
  expect(() => v.timestamp({ mode: "string" }).parse("2024-02-29T12:30:00.1234Z")).toThrow("precision");
  expect(() => v.timestamp({ withTimezone: false }).parse("2024-02-29T12:30:00Z")).toThrow();
  expect(() => v.timestamp({ mode: "string" }).parse("2024-02-29T12:30:00")).toThrow();
});
