import { query, mutation, publicTasks } from "./database-schema.ts";
import { objectInput } from "./standard.ts";
import { DatabaseError, v } from "../../src/index.ts";

export const byWorkspace = query({
  replica: publicTasks,
  input: objectInput({ workspace: v.string() }),
  handler: async ({ db, identity }, { workspace }) => {
    if (workspace !== identity) throw new Error("DATABASE_REPLICA_SERVER_GUARD");
    return db.select(publicTasks).where({ workspace });
  },
});

export const rename = mutation({
  input: objectInput({ id: v.uuid(), title: v.string(), version: v.int64({ mode: "bigint" }) }),
  handler: async ({ db, identity }, input) => {
    const row = await db.select("tasks").where({ id: input.id }).unique();
    if (!row || row.workspace !== identity) throw new DatabaseError({ code: "access_denied", message: "Task is unavailable" });
    if (row.version !== input.version) throw new DatabaseError({ code: "stale_task", message: "Task changed", details: { currentVersion: row.version.toString() } });
    return db.update("tasks", { title: input.title, version: row.version + 1n }).where({ id: input.id, version: input.version }).expectRows(1).returning(["id", "workspace", "title", "version"]);
  },
});
