import { sqliteFactory } from "./descriptor.ts";
export type { SQLiteConnection } from "./descriptor.ts";

export const sqlite = sqliteFactory(async () => { throw new Error("Use ctx.db inside a database declaration"); });
