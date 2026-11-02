import { defineSchema, defineTable, v } from "@hulla/db/schema";

export default defineSchema({
  projects: defineTable({
    slug: v.string(), name: v.string().min(1), description: v.string().nullable(),
  }, { primaryKey: "slug" }),
  tasks: defineTable({
    project: v.id("projects"),
    title: v.string().min(1).max(200),
    status: v.string().oneOf(["backlog", "in_progress", "done"]).default("backlog"),
    priority: v.int64().min(0).max(5).default(2),
    completed: v.boolean().default(false),
    due: v.date().nullable(),
    notes: v.string().nullable(),
    labels: v.json({ tags: v.array(v.string()) }),
  }).index(["project", "status"]),
});
