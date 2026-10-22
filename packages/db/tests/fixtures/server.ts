import { defineFunctions } from "../../src/declarations.ts";

export interface Note {
  id: number;
  title: string;
  owner: string;
}
export interface ReadContext {
  identity: string | null;
  db: { select(owner: string): Promise<Note[]> };
}
export interface WriteContext extends ReadContext {
  db: ReadContext["db"] & { insert(title: string, owner: string): Promise<Note> };
}

export const { query, mutation } = defineFunctions<ReadContext, WriteContext>();

// A browser build must never include this module or its Node-only dependency.
export { DatabaseSync } from "node:sqlite";
export const secret = "SERVER_IMPLEMENTATION_MUST_NOT_SHIP";
