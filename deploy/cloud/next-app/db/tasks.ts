import { query } from './_generated/functions.ts';
export const list = query(ctx => {
  if (!ctx.identity) throw new Error('Sign in required');
  return ctx.db.select('tasks').where({ owner: ctx.identity.id }).limit(50);
});
