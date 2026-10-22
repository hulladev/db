import { expect, test } from "bun:test";
import { transactionDatabase, type WriteDatabase } from "../src/database.ts";
import { defineSchema, defineTable, v } from "../src/schema.ts";
const schema = defineSchema({ users: defineTable({ email: v.string(), organization: v.string(), externalId: v.string(), count: v.int64({ mode: "bigint" }), active: v.boolean(), data: v.json() }, { unique: { email: { fields: ["email"] }, external: { fields: ["organization", "externalId"] } } }) });
const input = { email: "a", organization: "o", externalId: "e", count: 1n, active: true, data: {} };
const row = { row: { ...input, count: "1", id: "019a0000-0000-7000-8000-000000000001" }, version: "1" };
function database(request: (operation: Record<string, unknown>, validate?: (result: unknown) => void) => Promise<unknown>) { return transactionDatabase(schema, true, request) as WriteDatabase<typeof schema>; }

test("conflict policies capture atomic current/incoming expressions and skip returns only written rows", async () => {
  const requests: Record<string, unknown>[] = [];
  const db = database(async op => { requests.push(op); return op.conflict && (op.conflict as { action: { kind: string } }).action.kind === "skip" ? (Array.isArray(op.values) ? [] : null) : row; });
  const policyPatch = { active: false };
  const base = db.insert("users", input);
  const update = base.onConflict(({ on }) => on("email").update(policyPatch));
  policyPatch.active = true;
  const expressions = base.onConflict(({ on }) => on("organization", "externalId").update(({ current, incoming }) => ({ email: incoming.email, count: current.count.add(2n).add(incoming.count) })));
  const skip = base.onConflict(({ on }) => on("email").skip()).returning(["email"]);
  expect(requests).toHaveLength(0);
  await update;
  await expressions;
  expect(await skip).toBeNull();
  expect(await db.insert("users", [input]).onConflict(({ on }) => on("email").skip())).toEqual([]);
  expect((requests[0]?.conflict as { action: unknown }).action).toEqual({ kind: "update", values: { active: { kind: "value", value: false } } });
  expect(requests[1]?.conflict).toEqual({ fields: ["organization", "externalId"], action: { kind: "update", values: {
    email: { kind: "field", source: "incoming", field: "email" },
    count: { kind: "add", left: { kind: "add", left: { kind: "field", source: "current", field: "count" }, right: { kind: "value", value: "2" } }, right: { kind: "field", source: "incoming", field: "count" } },
  } } });
  expect(() => skip.expectRows(1)).toThrow("after execution");
});

test("write expressions reject invalid targets, primary-key writes and references escaping their callback", async () => {
  const requests: Record<string, unknown>[] = [];
  const db = database(async op => { requests.push(op); return []; });
  for (const fields of [["organization"], ["count"], ["email", "email"], ["email", "organization"]]) expect(() => db.insert("users", input).onConflict(({ on }) => (on as unknown as (...fields: string[]) => ReturnType<typeof on>)(...fields).skip())).toThrow();
  expect(() => db.insert("users", input).onConflict(({ on }) => on("email").update({ id: "bad" } as never))).toThrow("primary key");
  expect(() => db.insert("users", input).onConflict(() => ({}) as never)).toThrow("Return a conflict policy");
  let escaped: unknown;
  db.update("users", current => { escaped = current.count; return { count: current.count.add(1n) }; });
  expect(() => db.update("users", () => ({ count: escaped } as never))).toThrow("declaration callback");
  expect(() => db.update("users", () => ({ data: escaped } as never))).toThrow("declaration callback");
  expect(() => db.update("users", current => ({ count: current.email } as never))).toThrow("Incompatible");
  expect(() => db.update("users", current => ({ count: current.count.add(1 as never) }))).toThrow("int64");
  expect(() => db.update("users", current => {
    let value = current.count;
    for (let index = 0; index < 12; index++) value = value.add(value);
    return { count: value };
  })).toThrow("size exceeds budget");
  expect(requests).toHaveLength(0);
  await db.update("users", current => ({ count: current.count.add(1n), email: current.email })).where(true);
  expect(requests[0]?.values).toEqual({ count: { kind: "add", left: { kind: "field", source: "current", field: "count" }, right: { kind: "value", value: "1" } }, email: { kind: "field", source: "current", field: "email" } });
});

test("expectRows supplies a precommit check once per operation and counts skips, bulk and projections", async () => {
  let calls = 0, validations = 0, predicateCalls = 0;
  let result: unknown = [];
  const db = database(async (_op, validate) => { calls++; if (validate) { validations++; validate(result); } return result; });
  const zero = db.delete("users").expectRows(count => { predicateCalls++; return count === 0; }).where(true);
  await Promise.all([zero, zero]);
  expect([calls, validations, predicateCalls]).toEqual([1, 1, 1]);
  await expect(Promise.resolve(db.update("users", {}).where(true).returning(["email"]).expectRows(1))).rejects.toMatchObject({ code: "row_expectation_failed" });
  result = null;
  expect(await db.insert("users", input).onConflict(({ on }) => on("email").skip()).expectRows(0)).toBeNull();
  result = [row, row];
  expect(await db.insert("users", [input, input]).expectRows(count => count >= 2)).toHaveLength(2);
  result = [];
  await db.insert("users", []).expectRows(0);
  expect(calls).toBe(5);
  await db.insert("users", []);
  expect(calls).toBe(5);
  for (const bad of [-1, 0.5, Infinity, NaN]) expect(() => db.insert("users", input).expectRows(bad)).toThrow();
  await expect(Promise.resolve(db.delete("users").where(true).expectRows((async () => true) as never))).rejects.toThrow("synchronous boolean");
  const failure = new Error("predicate failed");
  await expect(Promise.resolve(db.delete("users").where(true).expectRows(() => { throw failure; }))).rejects.toBe(failure);
  await expect(Promise.resolve(db.delete("users").where(true).expectRows((() => Promise.resolve(true)) as never))).rejects.toThrow("synchronous boolean");
  await expect(Promise.resolve(db.delete("users").where(true).expectRows((() => 1) as never))).rejects.toThrow("synchronous boolean");
});
