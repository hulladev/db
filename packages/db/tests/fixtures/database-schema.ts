import { defineSchema, defineTable, v } from '../../src/index.ts';

export const schema = defineSchema({ tasks: defineTable({
  workspace: v.string(), title: v.string(), internalNotes: v.string(), version: v.int64({ mode: 'bigint' }).default(0n),
}) });
export const publicTasks = schema.tables.tasks.omit('internalNotes');
export const { query, mutation } = schema;
