// These callbacks also execute through the trusted TypeScript transaction worker.
import { query, mutation } from "../generated/deployed/functions.ts";
import { v } from "../../src/schema.ts";
import { objectInput } from "./standard.ts";
import type { Row } from "../generated/deployed/types.ts";
const authenticated = mutation.extend(ctx => {
  if (typeof ctx.identity !== "string") throw new Error("Sign in required");
  return { owner: ctx.identity };
});
export const list = query(async (ctx): Promise<Row<"entries">[]> => {
  if (typeof ctx.identity !== "string") throw new Error("Sign in required");
  return await ctx.db.select("entries").where({ owner: ctx.identity });
});
export const create = authenticated({
  input: objectInput({ title: v.string(), ordinal: v.int64({ mode: "bigint" }) }),
  handler: async (ctx, input): Promise<Row<"entries">> => await ctx.db.insert("entries", { ...input, owner: ctx.owner }),
});
export const fail = authenticated({
  input: objectInput({ title: v.string(), ordinal: v.int64({ mode: "bigint" }) }),
  handler: async (ctx, input): Promise<void> => {
    await ctx.db.insert("entries", { ...input, owner: ctx.owner });
    await ctx.db.insert("entries", { ...input, owner: ctx.owner });
  },
});

export const paginated = query({
  input: objectInput({ limit: { parse(value: unknown) { if (typeof value !== "number") throw new TypeError("Expected limit"); return value; } }, cursor: v.string().nullable() }),
  handler: async (ctx, input) => await ctx.db.select("entries").orderBy("ordinal").paginate(input),
});
