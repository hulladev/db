import { mutation } from "../generated/deployed/functions.ts";
import { standard } from "./standard.ts";

const guarded = mutation.extend({
  input: standard<{ title: string }>(value => {
    if (typeof value !== "object" || value === null || !("title" in value) || typeof value.title !== "string") throw new TypeError("Expected title");
    return { title: value.title.trim() };
  }),
  handler: async (ctx) => {
    if (typeof ctx.identity !== "string") throw new Error("Sign in required");
    await ctx.db.select("entries").where({ owner: ctx.identity });
    return { owner: ctx.identity };
  },
});
export const transformed = guarded({
  output: standard<{ title: string }, { label: string }>(async value => {
    await Promise.resolve();
    return { label: (value as { title: string }).title.toUpperCase() };
  }),
  handler: async (ctx, input) => ({ title: (await ctx.db.insert("entries", { title: input.title, owner: ctx.owner })).title }),
});
export const rejected = guarded({
  output: standard<{ title: string }, { label: string }>(async () => {
    await Promise.resolve();
    throw new TypeError("Output rejected after write");
  }),
  handler: async (ctx, input) => ({ title: (await ctx.db.insert("entries", { title: input.title, owner: ctx.owner })).title }),
});
