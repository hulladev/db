import { defineSchema, defineTable, v } from "../../src/schema.ts";
export default defineSchema({ entries: defineTable({ title: v.string(), owner: v.string(), ordinal: v.int64({ mode: "bigint" }).default(0n) }, { unique: { owner_title: { fields: ["owner", "title"] } } }) });
