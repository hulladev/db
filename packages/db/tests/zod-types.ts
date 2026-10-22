import { z } from "zod";
import { defineSchema, defineTable, v, type Id, type ResolveReferences } from "../src/schema.ts";
import type { WriteDatabase } from "../src/database.ts";
import { createInsertSchema, createSelectSchema, createUpdateSchema } from "../src/zod.ts";

const table = defineTable({ title: v.string(), note: v.string().nullable(), count: v.int64().default(1), at: v.timestamp(), big: v.int64({ mode: "bigint" }) }, { insertKeyPolicy: "generatedOnly" });
const insert = createInsertSchema(table, { title: value => value.min(2), note: value => value.max(10), count: value => value.nonnegative() });
type Input = z.infer<typeof insert>;
const valid: Input = { title: "Task", note: null, at: new Date(), big: 1n };
const count: number | undefined = valid.count;
void count;
// @ts-expect-error Generated-only primary keys are not an input field.
const forbiddenKey: Input = { ...valid, id: "id" };
// @ts-expect-error Nullable fields remain required in inserts.
const missingNote: Input = { title: "Task", at: new Date(), big: 1n };
// @ts-expect-error Representation modes remain visible in inferred function input.
const badBigint: Input = { ...valid, big: 1 };
const row = createSelectSchema(table);
const rowValue: z.infer<typeof row> = { ...valid, count: 1, id: "aaaaaaaa-0000-0000-0000-000000000001" };
void rowValue;
const update = createUpdateSchema(table);
const patch: z.infer<typeof update> = { note: null };
void patch;
// @ts-expect-error An update cannot assign the primary key.
const invalidPatch: z.infer<typeof update> = { id: "id" };
// @ts-expect-error Refinements cannot introduce an undeclared field.
createInsertSchema(table, { unknown: (value: z.ZodString) => value.min(1) });
class Money { readonly cents: number; constructor(cents: number) { this.cents = cents; } }
const custom = defineTable({ amount: v.number().codec({ encode: (value: Money) => value.cents, decode: cents => new Money(cents) }) });
const moneySchema = createInsertSchema(custom, { amount: () => z.instanceof(Money) });
const money: Money = moneySchema.parse({ amount: new Money(5) }).amount;
void money;
const textCodec = defineTable({ text: v.string().codec({ encode: (value: string) => value.toUpperCase(), decode: (value: string) => value.toLowerCase() }).nullable().default(null) });
createInsertSchema(textCodec, { text: schema => {
  // @ts-expect-error A custom codec requires an explicit native schema override.
  schema.min(1);
  return z.string().min(1);
} });
void [forbiddenKey, missingNote, badBigint, invalidPatch];

// Function validators produce ordinary scalars, while known IDs retain table provenance.
const folders = defineTable({ slug: v.string() }, { primaryKey: "slug" });
const otherFolders = defineTable({ slug: v.string() }, { primaryKey: "slug" });
const users = defineTable({ name: v.string() });
const relations = defineTable({ folder: v.id(folders), owner: v.id(users), label: v.string(), state: v.string().oneOf(["draft", "published"]).default("draft") });
const relational = defineSchema({ Folders: folders, OtherFolders: otherFolders, users, relations });
type RelationalSchema = ResolveReferences<typeof relational, { relations: { folder: "Folders"; owner: "users" } }>;
const relationInput = createInsertSchema(relations).omit({ owner: true });
const refinedState = createInsertSchema(relations, { state: value => value.min(1) });
const literalState: "draft" | "published" | undefined = refinedState.parse({ folder: "folder", owner: "owner", label: "label" }).state;
void literalState;
const transformedState = createInsertSchema(relations, { state: value => value.transform(state => {
  // @ts-expect-error The callback retains both allowed variants rather than widening to any.
  const draftOnly: "draft" = state;
  void draftOnly;
  return state;
}) });
function validatedDatabaseInput(db: WriteDatabase<RelationalSchema>, input: z.infer<typeof relationInput>, owner: Id<RelationalSchema, "users">, wrongFolder: Id<RelationalSchema, "OtherFolders">) {
  db.insert("relations", { ...input, owner });
  db.select("Folders").where("slug", "=", input.folder);
  // @ts-expect-error IDs from structurally identical tables must not be interchanged.
  db.insert("relations", { ...input, owner, folder: wrongFolder });
  // @ts-expect-error Known foreign IDs cannot be used to query another table's key.
  db.select("Folders").where("slug", "=", wrongFolder);
}
function transformedDatabaseInput(db: WriteDatabase<RelationalSchema>, input: z.infer<typeof transformedState>) {
  db.insert("relations", input);
}
void validatedDatabaseInput;
void transformedDatabaseInput;
