import { expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import type { BetterAuthOptions } from "better-auth";
import { getAuthTables, type BetterAuthDBSchema } from "better-auth/db";
import { compileSchema, defineSchema, parseRow, prepareInsert, type TableDefinition } from "@hulla/db/schema";
import { betterAuthTables, generateAuthSchema } from "../src/schema.ts";

test("configured and emitted auth tables preserve plugin data, physical names and database constraints", async () => {
  const options = {
    user: {
      modelName: "Members",
      fields: { email: "email_address", name: "display_name" },
      additionalFields: {
        tenant: { type: "string", required: true, fieldName: "tenant_key" },
        preferences: { type: "json", required: false },
      },
    },
    plugins: [{
      id: "audit-fixture",
      schema: {
        user: {
          fields: {},
          indexes: [{ fields: ["tenant", "email"], name: "member_tenant_email", unique: true }],
        },
        audit: {
          modelName: "AuthAudit",
          fields: {
            actor: { type: "string", fieldName: "actor_id", required: false, references: { model: "user", field: "id", onDelete: "set null" } },
            kind: { type: ["login", "logout"], required: true },
            labels: { type: "string[]", required: true },
            scores: { type: "number[]", required: true },
            count: { type: "number", bigint: true, required: true },
            payload: { type: "json", required: false },
            occurredAt: { type: "date", required: true, fieldName: "occurred_at", index: true },
          },
          indexes: [{ fields: ["actor", "occurredAt"], name: "audit_actor_time" }],
        },
      },
    }],
  } satisfies BetterAuthOptions;
  // A CLI-generated schema must load before any application database module exists.
  const directory = await mkdtemp(new URL("./generated-auth-", import.meta.url).pathname);
  try {
    const path = `${directory}/schema.ts`;
    await Bun.write(path, generateAuthSchema(getAuthTables(options)));
    const emitted = await import(path) as { authTables: Record<string, TableDefinition> };
    for (const tables of [betterAuthTables(options), emitted.authTables]) {
      const schema = defineSchema(tables);
      const compiled = compileSchema(schema);
      const member = compiled.find(table => table.name === "Members")!;
      const audit = compiled.find(table => table.name === "AuthAudit")!;
      expect(member.primaryKey).toBe("id");
      expect(member.keyGeneration).toBe("none");
      expect(member.fields.id!.scalar).toBe("string");
      expect(member.fields.email).toBeUndefined();
      expect(member.unique.member_tenant_email).toEqual({ fields: ["tenant_key", "email_address"], nulls: "distinct" });
      expect(Object.values(member.unique)).toContainEqual({ fields: ["email_address"], nulls: "distinct" });
      expect(compiled.find(table => table.name === "session")!.fields.userId!.target).toBe("Members");
      expect(compiled.find(table => table.name === "session")!.fields.userId!.onDelete).toBe("cascade");
      expect(audit.fields.actor_id!.target).toBe("Members");
      expect(audit.fields.actor_id!.onDelete).toBe("setNull");
      expect(audit.indexes).toContainEqual({ fields: ["actor_id", "occurred_at"], name: "audit_actor_time" });
      expect(audit.indexes.some(index => index.fields.length === 1 && index.fields[0] === "occurred_at")).toBe(true);
      const time = new Date("2026-01-02T03:04:05.006Z");
      const input = { id: "opaque-id", kind: "login", labels: ["trusted"], scores: [0.5, 1], count: 2, occurred_at: time };
      const row = prepareInsert(schema, "AuthAudit", input);
      expect(row).toMatchObject({ ...input, actor_id: null, payload: null });
      expect(audit.fields.occurred_at!.encode(time)).toBe("2026-01-02T03:04:05.006Z");
      expect(audit.fields.occurred_at!.decode("2026-01-02T03:04:05.006Z")).toEqual(time);
      expect(parseRow(schema, "AuthAudit", row)).toEqual(row);
      expect(() => prepareInsert(schema, "AuthAudit", { ...input, kind: "unknown" })).toThrow("oneOf");
      expect(() => prepareInsert(schema, "AuthAudit", { ...input, labels: [1] })).toThrow();
      expect(() => prepareInsert(schema, "AuthAudit", { ...input, occurred_at: time.toISOString() })).toThrow("Date");
      expect(() => prepareInsert(schema, "AuthAudit", { ...input, id: undefined })).toThrow("id");
      expect(() => prepareInsert(schema, "AuthAudit", { ...input, count: Number.MAX_SAFE_INTEGER + 1 })).toThrow("safe integer");
    }
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("auth schema rejects unsupported ID and foreign-key policies before generation", () => {
  expect(() => betterAuthTables({ advanced: { database: { generateId: "serial" } } })).toThrow("numeric IDs");
  const fields: BetterAuthDBSchema = {
    owner: { modelName: "Owner", fields: { email: { type: "string", unique: true } } },
    child: { modelName: "Child", fields: { owner: { type: "string", references: { model: "owner", field: "email" } } } },
  };
  expect(() => generateAuthSchema(fields)).toThrow("id primary key");
  fields.child!.fields.owner!.references = { model: "owner", field: "id", onDelete: "set default" };
  expect(() => generateAuthSchema(fields)).toThrow("set default");
  fields.child!.fields.owner!.references = { model: "owner", field: "id", onDelete: "set null" };
  expect(() => generateAuthSchema(fields)).toThrow("must be optional");
});
