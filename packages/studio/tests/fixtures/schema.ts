import { defineSchema, defineTable, v } from "@hulla/db/schema";

export default defineSchema({
  folders: defineTable({ slug: v.string() }, { primaryKey: "slug" }),
  entries: defineTable({
    folder: v.id("folders"), title: v.string().min(1).max(80),
    amount: v.int64({ mode: "bigint" }), done: v.boolean().default(false),
    day: v.date({ mode: "date" }).nullable(), details: v.json({ tags: v.array(v.string()) }),
    // Studio edits storage values while the public database uses application codecs.
    label: v.string().codec({ encode: (value: bigint) => value.toString(), decode: value => BigInt(value) }),
  }, { unique: { title: { fields: ["title"] } } }).index(["folder", "done"]),
  attachments: defineTable({ entry: v.id("entries", { onDelete: "cascade" }), body: v.string() }),
  counters: defineTable({ id: v.int64({ mode: "bigint" }), title: v.string() }, { keyGeneration: "none" }),
});
