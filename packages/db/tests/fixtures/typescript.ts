import { query as generatedQuery, mutation as generatedMutation } from "../generated/deployed/functions.ts";
import { v } from "../../src/schema.ts";
import { objectInput } from "./standard.ts";
const verify = (ctx: { identity: unknown }) => {
  if (typeof ctx.identity !== "string") throw new Error("Sign in required");
  return { owner: ctx.identity };
};
const query = generatedQuery.extend(verify);
const mutation = generatedMutation.extend(verify);
export const edit = mutation({ input: objectInput({ title: v.string() }), handler: async (ctx, input) => {
  const inserted = await ctx.db.insert("entries", { title: input.title, owner: ctx.owner });
  const fetched = (await ctx.db.select("entries").where({ id: inserted.id }).unique())!;
  const [updated] = await ctx.db.update("entries", { ordinal: (1n << 63n) - 1n }).where({ id: fetched.id });
  if (!updated) throw new Error("Expected updated row");
  await ctx.db.delete("entries").where({ id: updated.id });
  return { inserted, updated, absent: await ctx.db.select("entries").where({ id: updated.id }).unique() === null };
} });
export const crash = mutation({ input: objectInput({ title: v.string() }), handler: async (ctx, input): Promise<void> => {
  await ctx.db.insert("entries", { title: input.title, owner: ctx.owner });
  // Host process APIs are unavailable even after a database operation.
  process.exit(7);
} });
export const hang = mutation({ input: objectInput({ title: v.string() }), handler: async (ctx, input): Promise<void> => {
  await ctx.db.insert("entries", { title: input.title, owner: ctx.owner });
  await new Promise(() => {});
} });
export const invalid = mutation({ input: objectInput({ title: v.string() }), handler: async (ctx, input): Promise<string> => {
  await ctx.db.insert("entries", { title: input.title, owner: ctx.owner });
  return 123 as unknown as string;
} });
export const nonUnique = mutation({ input: objectInput({ title: v.string() }), handler: async (ctx, input): Promise<void> => {
  await ctx.db.insert("entries", [
    { title: input.title, owner: ctx.owner },
    { title: input.title + " other", owner: ctx.owner },
  ]);
  await ctx.db.select("entries").where({ owner: ctx.owner }).unique();
} });
export const leaked = mutation({ input: objectInput({ title: v.string() }), handler: async (ctx, input): Promise<void> => {
  // This intentionally violates context ownership. The worker must reject it.
  void ctx.db.insert("entries", { title: input.title, owner: ctx.owner }).then(() => {});
} });
export const queryWrite = query(async (ctx): Promise<void> => {
  // @ts-expect-error query contexts have no write capability
  await ctx.db.insert("entries", { title: "forbidden", owner: ctx.owner });
});
export const environment = query(() => typeof process === "undefined" ? "absent" : "exposed");
export const parallel = query(async ctx => {
  const [first, second] = await Promise.all([
    ctx.db.select("entries").where({ owner: ctx.owner }),
    ctx.db.select("entries").where({ owner: ctx.owner }),
  ]);
  return { first, second };
});


// Recursive JSON aliases must remain portable across generated function boundaries.
export const echoJson = query({ input: objectInput({ payload: v.json() }), handler: (_ctx, input) => input.payload });
