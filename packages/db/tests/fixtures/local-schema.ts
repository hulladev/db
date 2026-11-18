import { defineSchema, defineTable, v } from '../../src/index.ts';
export const schema = defineSchema({ tasks: defineTable({ title: v.string() }) });
