import { expect, test } from "bun:test";
import { z } from "zod";
import { defineSchema, defineTable, v } from "../src/schema.ts";
import { createInsertSchema, createSelectSchema, createUpdateSchema } from "../src/zod.ts";

test("Zod row schemas preserve database omission rules without inserting defaults or accepting immutable keys", () => {
  const table = defineTable({ title: v.string().min(1), note: v.string().nullable(), count: v.int64().default(2) }, { insertKeyPolicy: "generatedOnly" });
  const insert = createInsertSchema(table);
  const input = { title: "Task", note: null };
  expect(insert.parse(input)).toEqual(input);
  expect(insert.safeParse({ title: "Task" }).success).toBe(false);
  expect(insert.safeParse({ ...input, id: "aaaaaaaa-0000-0000-0000-000000000001" }).success).toBe(false);
  expect(insert.safeParse({ ...input, unknown: 1 }).success).toBe(false);
  const update = createUpdateSchema(table);
  expect(update.parse({ count: undefined, note: null })).toEqual({ count: undefined, note: null });
  expect(update.safeParse({ id: undefined }).success).toBe(false);
  const select = createSelectSchema(table);
  expect(select.safeParse({ ...input, count: 2 }).success).toBe(false);
  expect(select.parse({ ...input, count: 2, id: "aaaaaaaa-0000-0000-0000-000000000001" }).count).toBe(2);
  expect(insert.extend({ extra: z.boolean() }).parse({ ...input, extra: true }).extra).toBe(true);
});

test("Zod field validation preserves codepoint bounds, oneOf and numeric modes without coercion", () => {
  const table = defineTable({ title: v.string().min(1).max(1), role: v.string().oneOf(["admin", "user"]), safe: v.int64().min(2).max(4), big: v.int64({ mode: "bigint" }).min(5n), score: v.number().min(0).max(1) });
  const schema = createInsertSchema(table);
  const input = { title: "😀", role: "user" as const, safe: 3, big: 5n, score: 0.5 };
  expect(schema.parse(input)).toEqual(input);
  for (const patch of [{ title: "" }, { title: "😀😀" }, { role: "owner" }, { safe: 1 }, { safe: "3" }, { safe: Number.MAX_SAFE_INTEGER + 1 }, { big: 5 }, { big: 4n }, { score: 2 }]) expect(schema.safeParse({ ...input, ...patch }).success).toBe(false);
});

test("native Zod refinements retain nullable and optional wrapping while database constraints remain authoritative", () => {
  const table = defineTable({ title: v.string().min(3), note: v.string().nullable(), count: v.number().default(1) });
  const schema = createInsertSchema(table, { title: string => string.max(5), note: string => string.max(4), count: number => number.min(0) });
  expect(schema.parse({ title: "Task", note: null })).toEqual({ title: "Task", note: null });
  expect(schema.safeParse({ title: "No", note: "ok" }).success).toBe(false);
  expect(schema.safeParse({ title: "Task too long", note: "ok" }).success).toBe(false);
  expect(schema.safeParse({ title: "Task", note: "Too long" }).success).toBe(false);
  const replaced = createInsertSchema(table, { title: () => z.string() });
  expect(replaced.safeParse({ title: "No", note: null }).success).toBe(false);
});

test("codecs validate application objects and nested JSON values without encoding the returned input", () => {
  class Money { readonly cents: number; constructor(cents: number) { this.cents = cents; } }
  const table = defineTable({
    amount: v.number().min(1).codec({ encode: (money: Money) => { if (!(money instanceof Money)) throw new TypeError("Expected Money"); return money.cents; }, decode: cents => new Money(cents) }),
    info: v.json({ ordinal: v.string().codec({ encode: (value: bigint) => { if (typeof value !== "bigint") throw new TypeError("Expected bigint"); return value.toString(); }, decode: BigInt }), tags: v.array(v.string()), note: v.string().optional() }),
  });
  const schema = createInsertSchema(table, { amount: () => z.instanceof(Money).refine(money => money.cents <= 10) });
  const amount = new Money(5);
  const parsed = schema.parse({ amount, info: { ordinal: 42n, tags: ["ok"] } });
  expect(parsed.amount).toBe(amount);
  expect(parsed.info.ordinal).toBe(42n);
  expect(schema.safeParse({ amount: new Money(0), info: { ordinal: 42n, tags: ["ok"] } }).success).toBe(false);
  expect(schema.safeParse({ amount: new Money(11), info: { ordinal: 42n, tags: ["ok"] } }).success).toBe(false);
  expect(schema.safeParse({ amount, info: { ordinal: "42", tags: ["ok"] } }).success).toBe(false);
  expect(schema.safeParse({ amount, info: { ordinal: 42n, tags: [1] } }).success).toBe(false);
  expect(schema.safeParse({ amount, info: { ordinal: 42n, tags: ["ok"], unknown: true } }).success).toBe(false);
  const primitive = defineTable({ text: v.string().min(2).codec({ encode: (value: string) => value.toUpperCase(), decode: (value: string) => value.toLowerCase() }).nullable() });
  const primitiveSchema = createInsertSchema(primitive, { text: () => z.string().max(3) });
  expect(primitiveSchema.parse({ text: "ok" })).toEqual({ text: "ok" });
  expect(primitiveSchema.parse({ text: null })).toEqual({ text: null });
  expect(primitiveSchema.safeParse({ text: "x" }).success).toBe(false);
});

test("temporal modes validate exact application representations and reject lost precision", () => {
  const table = defineTable({ day: v.date(), dayObject: v.date({ mode: "date" }), at: v.timestamp(), precise: v.timestamp({ mode: "string", precision: 6 }), local: v.timestamp({ withTimezone: false }) });
  const schema = createInsertSchema(table);
  const input = { day: "2026-10-20", dayObject: new Date("2026-10-20T00:00:00Z"), at: new Date("2026-10-20T12:30:00Z"), precise: "2026-10-20T12:30:00.123456Z", local: "2026-10-20T12:30:00.123" };
  expect(schema.parse(input)).toEqual(input);
  for (const patch of [{ day: "2026-02-30" }, { dayObject: "2026-10-20" }, { at: input.at.toISOString() }, { precise: "2026-10-20T12:30:00.1234567Z" }, { local: "2026-10-20T12:30:00.123Z" }]) expect(schema.safeParse({ ...input, ...patch }).success).toBe(false);
});

test("foreign key schemas resolve actual target key representations instead of guessing UUID", () => {
  const users = defineTable({ number: v.int64() }, { primaryKey: "number", keyGeneration: "none" });
  const posts = defineTable({ owner: v.id("users"), secondary: v.id(users) });
  expect(() => createInsertSchema(posts)).toThrow("defineSchema");
  defineSchema({ users, posts });
  const schema = createInsertSchema(posts);
  expect(schema.parse({ owner: 2, secondary: 3 })).toEqual({ owner: 2, secondary: 3 });
  expect(schema.safeParse({ owner: "2", secondary: 3 }).success).toBe(false);
});
