import { defineSchema, defineTable, sql, v } from "../../src/schema.ts";

// A user-defined publishing schema used across SDK and native integration tests.
export default defineSchema(
  {
    publications: defineTable(
      {
        slug: v.string(),
        title: v.string(),
      },
      { primaryKey: "slug" },
    ),
    articles: defineTable(
      {
        publication: v.id("publications", { onDelete: "cascade" }),
        title: v.string().min(1).max(200),
        published: v.boolean().default(false),
        excerpt: v.string().nullable(),
        ordinal: v.int64({ mode: "bigint" }).default(9223372036854775807n),
        minimum: v.int64({ mode: "bigint" }).default(-(1n << 63n)),
        origin: v.uuid().default("AAAAAAAA-0000-0000-0000-000000000001"),
        label: v.string().default('line\n"quote"\\path\u0000🦀'),
      },
      {
        insertKeyPolicy: "generatedOnly",
        checks: fields => ({ ordinal_range: sql`${fields.ordinal} >= ${fields.minimum} AND ${fields.ordinal} >= ${0n}` }),
        unique: {
          publication_title: { fields: ["publication", "title"] },
          excerpt: { fields: ["excerpt"], nulls: "distinct" },
        },
      },
    ).index(["publication", "published"]),
    counters: defineTable(
      { id: v.int64({ mode: "bigint" }), value: v.int64({ mode: "bigint" }) },
      {
        keyGeneration: { handler: "counters.next", outputType: "int64" },
      },
    ),
  },
  { defaultUuidGeneration: "v4" },
);
