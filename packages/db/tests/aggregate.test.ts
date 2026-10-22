import { expect, test } from "bun:test";
import { transactionDatabase, type ReadDatabase } from "../src/database.ts";
import { compileSchema, defineSchema, defineTable, v } from "../src/schema.ts";
import { createTables } from "../src/fields.ts";

const schema = defineSchema({
  orders: defineTable({ owner: v.string(), amount: v.int64({ mode: "bigint" }).nullable(), score: v.number(), settings: v.json({ theme: v.string() }) }),
});
const tables = createTables<typeof schema>(Object.fromEntries(compileSchema(schema).map(table => [table.name, table.fields])));
function database(request: (op: Record<string, unknown>) => Promise<unknown>) {
  return transactionDatabase(schema, false, request) as ReadDatabase<typeof schema>;
}

test("grouped aggregates keep row and group filters separate and decode sums beyond int64 without rounding", async () => {
  const requests: Record<string, unknown>[] = [];
  const db = database(async operation => {
    requests.push(operation);
    return [{ owner: "alice", total: 3, sum: "18446744073709551614", average: "6148914691236517204.6667" }];
  });
  const aggregate = db.select("orders").where("score", ">=", 0).groupBy("owner")
    .aggregate(a => ({ total: a.count(), sum: a.sum(tables.orders.amount), average: a.avg("amount") }));
  const branch = aggregate.having(({ and, gte }) => and(gte("sum", 9223372036854775808n), gte("average", "2.25")))
    .orderBy("total", "desc").limit(2);
  expect(requests).toHaveLength(0);
  const result = await branch;
  expect(result).toEqual([{ owner: "alice", total: 3, sum: 18446744073709551614n, average: "6148914691236517204.6667" }]);
  expect(await branch).toBe(result);
  expect(requests).toHaveLength(1);
  expect(requests[0]).toMatchObject({
    method: "aggregate", table: "orders", groupBy: ["owner"],
    aggregates: { total: { op: "count" }, sum: { op: "sum", field: "amount" }, average: { op: "avg", field: "amount" } },
    filters: [{ kind: "compare", field: "score", operator: ">=", operand: { kind: "value", value: 0 } }],
    having: [{ kind: "and", predicates: [
      { kind: "compare", field: "sum", operator: ">=", operand: { kind: "value", value: "9223372036854775808" } },
      { kind: "compare", field: "average", operator: ">=", operand: { kind: "value", value: "2.25" } },
    ] }],
    order: [{ field: "total", direction: "desc" }], limit: 2,
  });
  await aggregate;
  expect(requests[1]?.having).toEqual([]);
  expect(() => aggregate.orderBy("owner")).toThrow("after execution");
});

test("aggregate transport rejects precision loss, malformed values and wrong output shapes", async () => {
  const output: unknown[] = [
    { total: 0, sum: null, average: null, score: null },
    { total: Number.MAX_SAFE_INTEGER + 1, sum: null, average: null, score: null },
    { total: 1, sum: 12, average: "12", score: 0 },
    { total: 1, sum: "12", average: "NaN", score: 0 },
    { total: 1, sum: "12", average: "12", score: Infinity },
    { total: 1, sum: "12", average: "12" },
  ];
  const db = database(async () => output.shift());
  const summary = () => db.select("orders").aggregate(a => ({ total: a.count(), sum: a.sum("amount"), average: a.avg("amount"), score: a.avg("score") }));
  expect(await summary()).toEqual({ total: 0, sum: null, average: null, score: null });
  for (const message of ["safe integer", "bigint", "decimal", "number", "requested fields"]) {
    await expect(Promise.resolve(summary())).rejects.toThrow(message);
  }
});

test("aggregate construction rejects ambiguous row semantics and invalid aliases before submitting work", () => {
  const db = database(async () => { throw new Error("Unexpected execution"); });
  expect(() => db.select("orders", ["owner"]).aggregate(a => ({ total: a.count() }))).toThrow("row projection");
  expect(() => db.select("orders").orderBy("score").aggregate(a => ({ total: a.count() }))).toThrow("ordering after");
  expect(() => db.select("orders").groupBy("owner", "owner").aggregate(a => ({ total: a.count() }))).toThrow("Duplicate grouping");
  expect(() => db.select("orders").groupBy("owner").aggregate(a => ({ owner: a.count() }) as never)).toThrow("collides");
  expect(() => db.select("orders").aggregate(() => ({ total: {} }) as never)).toThrow("Invalid aggregate");
  const grouped = db.select("orders").groupBy("owner").aggregate(a => ({ average: a.avg("amount") }));
  expect(() => grouped.having(({ startsWith }) => (startsWith as Function)("average", "2"))).toThrow("string field");
  expect(() => grouped.having(({ gte }) => gte("average", "not numeric"))).toThrow("decimal");
  expect(() => grouped.orderBy("amount" as never)).toThrow("aggregate alias");
});
