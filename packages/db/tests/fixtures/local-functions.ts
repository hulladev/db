import { schema } from './local-schema.ts';
import { v } from '../../src/index.ts';
import { objectInput } from './standard.ts';
export const list = schema.query(({ db }) => db.select('tasks'));
export { list as allTasks };
export const create = schema.mutation({
  input: objectInput({ title: v.string().min(1) }),
  handler: ({ db }, input) => db.insert('tasks', input),
});
export const invalidOutput = schema.mutation({
  output: objectInput({ value: v.string() }),
  handler: async ({ db }) => {
    await db.insert('tasks', { title: 'Must roll back' });
    return { value: 123 } as never;
  },
});
