import { defineSchema, defineTable, v } from '@hulla/db/schema';
export default defineSchema({ tasks: defineTable({ title: v.string(), owner: v.string() }) });
