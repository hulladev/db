import { expect, test } from "bun:test";
import { compileSchema, defineSchema, defineTable, prepareInsert, v } from "../src/schema.ts";

test("column constraints validate encoded values without restricting predicate or arithmetic operands", () => {
  const money = v.number().min(10).max(100).codec({ encode: (value: { cents: number }) => value.cents, decode: cents => ({ cents }) });
  expect(money.encode({ cents: 20 })).toBe(20);
  expect(() => money.encode({ cents: 9 })).toThrow("minimum");
  expect(() => money.decode(101)).toThrow("maximum");
  expect(money.encodeOperand({ cents: -1 })).toBe(-1);
  expect(money.storage(-1)).toBe(-1);
  const text = v.string().min(1).max(2);
  expect(text.parse("😀a")).toBe("😀a");
  expect(() => text.parse("😀ab")).toThrow("maximum");
  const small = v.int64().min(1).max(5n);
  expect(small.constraints).toEqual({ min: 1n, max: 5n });
  expect(() => small.parse(0)).toThrow("minimum");
  const options = ["draft", "published"];
  const state = v.string().oneOf(options);
  options.push("deleted");
  expect(() => state.parse("deleted")).toThrow("oneOf");
  expect(() => state.default("deleted")).toThrow("oneOf");
  expect(() => v.string().oneOf([])).toThrow("nonempty");
  expect(() => v.string().min(3).max(2)).toThrow("Contradictory");
  expect(() => v.number().oneOf([1, 5]).max(3)).toThrow("maximum");
  expect(() => v.string().default("short").min(10)).toThrow("minimum");
  expect(() => v.int64().max(Number.MAX_SAFE_INTEGER + 1)).toThrow("safe integers");
  expect(() => v.json({ nested: v.string().min(1) })).toThrow("JSON members");
});

test("references inherit registered key representations and resolve cyclic non-key links", () => {
  const users = defineTable({ handle: v.string() }, { primaryKey: "handle" });
  const counters = defineTable({ id: v.int64({ mode: "bigint" }) });
  const posts = defineTable({ author: v.id(users), sequence: v.id("counters"), parent: v.id("posts").nullable() });
  const schema = defineSchema({ users, counters, posts });
  const post = prepareInsert(schema, "posts", { author: "alice", sequence: 2n, parent: null });
  expect(String(post.author)).toBe("alice");
  expect(BigInt(post.sequence)).toBe(2n);
  const compiled = compileSchema(schema).find(table => table.name === "posts")!;
  expect(compiled.fields.author!.target).toBe("users");
  expect(compiled.fields.author!.scalar).toBe("string");
  expect(compiled.fields.sequence!.mode).toBe("bigint");
  expect(compiled.fields.parent!.onDelete).toBe("restrict");
  expect(() => v.id(users).codec({ encode: String, decode: String })).toThrow("primary key codec");
  expect(() => defineSchema({ users, posts: defineTable({ author: v.id(users, { onDelete: "setNull" }) }) })).toThrow("nullable");
  const nullable = defineSchema({ users, posts: defineTable({ author: v.id(users, { onDelete: "setNull" }).nullable() }) });
  expect(compileSchema(nullable)[1]!.fields.author!.onDelete).toBe("setNull");
  const defaults = defineSchema({ users, posts: defineTable({ author: v.id("users").default("alice") }) });
  expect(prepareInsert(defaults, "posts", {}).author).toBe("alice" as never);
});

test("one-to-one primary-key references require existing keys and reject key-only cycles", () => {
  const users = defineTable({ key: v.int64({ mode: "bigint" }) }, { primaryKey: "key" });
  const profiles = defineTable({ id: v.id(users), bio: v.string() });
  const settings = defineTable({ id: v.id("profiles"), theme: v.string() });
  const schema = defineSchema({ users, profiles, settings });
  expect(compileSchema(schema).map(table => table.keyGeneration)).toEqual(["none", "none", "none"]);
  expect(BigInt(prepareInsert(schema, "settings", { id: 5n, theme: "dark" }).id)).toBe(5n);
  expect(() => prepareInsert(schema, "profiles", { bio: "missing key" })).toThrow("Missing field");
  expect(() => defineSchema({ a: defineTable({ id: v.id("b") }), b: defineTable({ id: v.id("a") }) })).toThrow("Cyclic primary-key");
});

test("a reused table cannot silently change its resolved foreign key validator", () => {
  const posts = defineTable({ author: v.id("users") });
  defineSchema({ users: defineTable({ id: v.string() }), posts });
  // Separate compatible registries can share the declaration without changing its meaning.
  expect(() => defineSchema({ users: defineTable({ id: v.string() }), posts })).not.toThrow();
  expect(() => defineSchema({ users: defineTable({ id: v.int64({ mode: "bigint" }) }), posts })).toThrow("incompatible schema");
});

test("object and string references consistently keep field constraints on the target key", () => {
  const users = defineTable({ id: v.string().min(1) });
  for (const reference of [v.id(users), v.id("users")]) {
    const untyped = reference as unknown as { min(value: number): unknown; max(value: number): unknown; oneOf(values: readonly string[]): unknown };
    expect(() => untyped.min(1)).toThrow("target primary key or a named SQL check");
    expect(() => untyped.max(10)).toThrow("target primary key or a named SQL check");
    expect(() => untyped.oneOf(["alice"])).toThrow("target primary key or a named SQL check");
  }
});

test("indexes snapshot authoring inputs and reject duplicate or unknown definitions before DDL", () => {
  const fields: ("owner" | "created")[] = ["owner", "created"];
  const base = defineTable({ owner: v.string(), created: v.int64() });
  const indexed = base.index(fields).index(["created"], { name: "creation_order" });
  fields.pop();
  expect(base.indexes).toEqual([]);
  const table = compileSchema(defineSchema({ tasks: indexed }))[0]!;
  expect(table.indexes).toEqual([{ name: "tasks_owner_created_idx", fields: ["owner", "created"] }, { name: "creation_order", fields: ["created"] }]);
  expect(() => defineSchema({ tasks: indexed.index(["owner", "created"]) })).toThrow("Duplicate index");
  expect(() => base.index(["missing"] as never)).toThrow("Unknown index field");
  expect(() => defineSchema({ first: base.index(["owner"], { name: "shared" }), second: base.index(["owner"], { name: "shared" }) })).toThrow("another table");
  const long = compileSchema(defineSchema({ ["a".repeat(100)]: base.index(["owner"]) }))[0]!.indexes[0]!.name;
  expect(new TextEncoder().encode(long).length).toBeLessThanOrEqual(63);
});
