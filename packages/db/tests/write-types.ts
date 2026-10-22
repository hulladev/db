import { defineSchema, defineTable, v } from "../src/schema.ts";
import type { WriteDatabase, DatabaseOperation } from "../src/database.ts";
const schema = defineSchema({ users: defineTable({ email: v.string(), organization: v.string(), externalId: v.string(), count: v.int64({ mode: "bigint" }), score: v.number(), optional: v.int64({ mode: "bigint" }).nullable() }, { unique: { email: { fields: ["email"] }, external: { fields: ["organization", "externalId"] } } }) });
declare const db: WriteDatabase<typeof schema>;
const input = { email: "a", organization: "o", externalId: "e", count: 0n, score: 0, optional: null };
const normal: DatabaseOperation<{ email: string }> = db.insert("users", input).returning(["email"]);
const skipped: DatabaseOperation<{ email: string } | null> = db.insert("users", input).onConflict(({ on }) => on("email").skip()).returning(["email"]).expectRows(count => count <= 1);
const bulk: DatabaseOperation<{ email: string }[]> = db.insert("users", [input]).onConflict(({ on }) => on("organization", "externalId").skip()).returning(["email"]);
void [normal, skipped, bulk];
db.insert("users", input).onConflict(({ on }) => on("id").update(({ current, incoming }) => ({ email: incoming.email, count: current.count.add(1n), optional: current.optional.add(incoming.optional) })));
db.update("users", current => ({ count: current.count.add(2n), score: current.score.add(0.5), optional: current.count.add(current.optional) })).expectRows(1).where(true);
db.delete("users").returning(["email"]).expectRows(1).where(true);
// @ts-expect-error A skipped single insert is nullable even through projections.
const nonnull: DatabaseOperation<{ email: string }> = skipped;
void nonnull;
// @ts-expect-error A member of a composite unique key is not independently unique.
db.insert("users", input).onConflict(({ on }) => on("organization").skip());
// @ts-expect-error Unconstrained fields are not conflict targets.
db.insert("users", input).onConflict(({ on }) => on("count").skip());
// @ts-expect-error The complete composite tuple is required.
db.insert("users", input).onConflict(({ on }) => on("organization", "count").skip());
// @ts-expect-error Primary keys cannot be changed by a conflict update.
db.insert("users", input).onConflict(({ on }) => on("email").update(({ incoming }) => ({ id: incoming.id })));
// @ts-expect-error Bigint and number arithmetic cannot be mixed.
db.update("users", current => ({ count: current.count.add(1) })).where(true);
// @ts-expect-error String fields have no numeric methods.
db.update("users", current => ({ email: current.email.add(1) })).where(true);
// @ts-expect-error Field assignments preserve their value types.
db.update("users", current => ({ count: current.email })).where(true);
// @ts-expect-error Nullable expressions cannot silently fill nonnullable fields.
db.update("users", current => ({ count: current.optional })).where(true);
// @ts-expect-error The expectation must finish synchronously.
db.delete("users").where(true).expectRows(async count => count > 0);
// @ts-expect-error Expectations must return boolean.
db.insert("users", input).expectRows(count => count);
// @ts-expect-error Expecting rows never removes the requirement to scope a write.
const unscoped: DatabaseOperation<unknown> = db.update("users", {}).expectRows(1);
void unscoped;
