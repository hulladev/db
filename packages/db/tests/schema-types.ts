import {
  defineSchema,
  defineTable,
  parseRow,
  v,
  type Insert as SchemaInsert,
} from "../src/schema.ts";
import type { Id, Insert, Patch, Row } from "./generated/types.ts";
import type { QueryContext, MutationContext } from "../src/server.ts";
import authoredSchema from "./fixtures/schema.ts";

function contracts(
  article: Row<"articles">,
  publicationId: Id<"publications">,
) {
  const input: Insert<"articles"> = {
    publication: publicationId,
    title: "Draft",
    excerpt: null,
  };
  const counter: Insert<"counters"> = { value: 1n }; // Custom-generated key is optional.
  const patch: Patch<"articles"> = { title: undefined, excerpt: null, ordinal: -1n };
  const ordinal: bigint = article.ordinal;
  const id: Id<"articles"> = article.id;
  // @ts-expect-error IDs are tied to their table
  const wrongId: Id<"articles"> = publicationId;
  // @ts-expect-error generated-only keys cannot be supplied
  const explicitId: Insert<"articles"> = { ...input, id };
  // @ts-expect-error nullable does not permit omission
  const missingNullable: Insert<"articles"> = {
    publication: publicationId,
    title: "Draft",
  };
  // @ts-expect-error required custom string primary key
  const publication: Insert<"publications"> = { title: "Daily" };
  // @ts-expect-error updates cannot assign a primary key
  const keyPatch: Patch<"articles"> = { id };
  // @ts-expect-error int64 uses bigint, never lossy JS numbers
  const lossy: Patch<"articles"> = { ordinal: 1 };
  // @ts-expect-error unknown table
  const unknown: Row<"tasks"> = article;
  // @ts-expect-error unknown field
  const unknownField: Patch<"articles"> = { typo: true };
  // @ts-expect-error primary key must name a declared field or the implicit id
  defineTable({ name: v.string() }, { primaryKey: "missing" });
  defineTable(
    { name: v.string() },
    // @ts-expect-error unique constraints must name declared fields
    { unique: { name: { fields: ["missing"] } } },
  );
  const explicit = defineSchema({
    records: defineTable({ id: v.uuid(), text: v.string() }),
    manual: defineTable({ id: v.uuid() }, { keyGeneration: "none" }),
  });
  const generatedKey: SchemaInsert<typeof explicit, "records"> = { text: "x" };
  // @ts-expect-error disabled UUID generation requires an explicit ID
  const missingKey: SchemaInsert<typeof explicit, "manual"> = {};
  const row = parseRow(explicit, "records", {
    id: "00000000-0000-0000-0000-000000000001",
    text: "x",
  });
  // @ts-expect-error fields preserve declared scalar types
  const invalidText: number = row.text;
  return {
    input,
    counter,
    patch,
    ordinal,
    id,
    wrongId,
    explicitId,
    missingNullable,
    publication,
    keyPatch,
    lossy,
    unknown,
    unknownField,
    invalidText,
    generatedKey,
    missingKey,
  };
}
void contracts;

function callbackContexts(
  query: QueryContext<typeof authoredSchema, string>,
  mutation: MutationContext<typeof authoredSchema, string>,
  article: Row<"articles">,
  publication: Row<"publications">,
) {
  mutation.db.update("articles", { excerpt: null, ordinal: 2n }).where({ id: article.id });
  query.db.select("articles").where({ publication: publication.slug });
  // @ts-expect-error query callbacks cannot mutate storage
  query.db.insert("publications", { slug: "daily", title: "Daily" });
  // @ts-expect-error table IDs cannot be substituted across tables
  query.db.select("articles").where("id", "=", publication.slug);
  // @ts-expect-error unknown tables fail at authoring time
  query.db.select("missing");
  // @ts-expect-error patches cannot change primary keys
  mutation.db.update("articles", { id: article.id });
  // @ts-expect-error int64 fields do not accept lossy numbers
  mutation.db.update("articles", { ordinal: 1 });
  // @ts-expect-error selectors use the selected table's fields
  query.db.select("publications").where({ published: true });
  // @ts-expect-error unscoped writes cannot be consumed as database operations
  mutation.db.delete("articles").then(() => {});
  // @ts-expect-error field comparisons preserve the selected field value type
  query.db.select("articles").where("ordinal", "=", "1");
  // @ts-expect-error limit is terminal
  query.db.select("articles").limit(1).where({ title: "Draft" });
}
void callbackContexts;

function paginationTerminal(query: import("../src/database.ts").ReadDatabase<typeof authoredSchema>) {
  const page = query.select("articles").paginate({ limit: 20, cursor: null });
  // @ts-expect-error pagination is terminal
  page.limit(1);
  // @ts-expect-error pagination cannot be chained
  page.paginate({ limit: 20 });
  // @ts-expect-error limit is terminal and cannot be paginated
  query.select("articles").limit(1).paginate({ limit: 20 });
  // @ts-expect-error unique is terminal and cannot be paginated
  query.select("articles").unique().paginate({ limit: 20 });
}
void paginationTerminal;

function schemaAuthoring() {
  const tasks = defineTable({ title: v.string().min(1), state: v.string().oneOf(["draft", "published"]).default("draft"), count: v.int64(), note: v.string().nullable() });
  const row: typeof tasks.$inferSelect = { id: "id", title: "title", state: "draft", count: 1, note: null };
  const insert: typeof tasks.$inferInsert = { title: "title", count: 1, note: null };
  const patch: typeof tasks.$inferUpdate = { state: "published", count: undefined };
  // @ts-expect-error oneOf preserves the allowed literal union
  const invalidState: typeof tasks.$inferUpdate = { state: "deleted" };
  // @ts-expect-error updates cannot replace keys
  const invalidKey: typeof tasks.$inferUpdate = { id: "id" };
  // @ts-expect-error only string and numeric columns accept bounds
  v.boolean().min(1);
  // @ts-expect-error JSON columns do not accept scalar oneOf constraints
  v.json().oneOf([{}]);
  // @ts-expect-error unknown index columns fail before generation
  tasks.index(["unknown"]);
  defineTable({ slug: v.string() }, { primaryKey: "slug" }).index(["slug"]);
  // @ts-expect-error no implicit id exists when a custom key is used
  defineTable({ slug: v.string() }, { primaryKey: "slug" }).index(["id"]);
  const users = defineTable({ name: v.string() });
  const projects = defineTable({ name: v.string() });
  // @ts-expect-error foreign key bounds belong to the target key or a SQL check
  v.id(users).min(1);
  // @ts-expect-error string references use the same constraint ownership as object references
  v.id("users").max(10);
  // @ts-expect-error foreign key subset constraints require a named SQL check
  v.id(users).oneOf(["alice"]);
  // @ts-expect-error string reference enums cannot bypass target key constraint ownership
  v.id("users").oneOf(["alice"]);
  const linked = defineSchema({ users, projects, links: defineTable({ owner: v.id(users) }) });
  // Generated code supplies this exact identity map, even when table shapes are identical.
  type Bound = import("../src/schema.ts").ResolveReferences<typeof linked, { links: {owner: "users"} }>;
  type Owner = import("../src/schema.ts").Row<Bound, "links">["owner"];
  function identities(user: import("../src/schema.ts").Id<Bound, "users">, project: import("../src/schema.ts").Id<Bound, "projects">) {
    const valid: Owner = user;
    // @ts-expect-error structurally identical tables still have distinct generated ID brands
    const invalid: Owner = project;
    return { valid, invalid };
  }
  return { row, insert, patch, invalidState, invalidKey, identities };
}
void schemaAuthoring;

function jsonColumnTypes() {
  const table = defineTable({ profile: v.object({ name: v.string(), age: v.number().optional() }), tags: v.array(v.string()).nullable() });
  const row: typeof table.$inferInsert = { profile: { name: "Ada" }, tags: null };
  const tags: typeof table.$inferUpdate = { tags: ["one"] };
  // @ts-expect-error JSON shorthand retains nested field types
  const invalid: typeof table.$inferInsert = { profile: { name: 42 }, tags: [] };
  // @ts-expect-error JSON columns remain required unless defaulted
  const missing: typeof table.$inferInsert = { tags: [] };
  // @ts-expect-error optional describes JSON members, not nullable database columns
  defineTable({ profile: v.object({ name: v.string() }).optional() });
  return [row, tags, invalid, missing];
}
void jsonColumnTypes;
