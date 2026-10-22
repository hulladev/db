import { mutation, query, secret } from "./server.ts";
import { v } from "../../src/schema.ts";
import { objectInput } from "./standard.ts";

export const pending = query(async ({ db, identity }) => {
  if (!identity) throw new Error("Sign in required");
  return db.select(identity);
});

export const create = mutation({
  input: objectInput({ title: v.string() }),
  handler: async ({ db, identity }, { title }) => {
    if (!identity) throw new Error("Sign in required");
    const trimmed = title.trim();
    if (!trimmed) throw new Error("Title required");
    return db.insert(trimmed, identity);
  },
});

export const serverOnlyHelper = () => secret;
