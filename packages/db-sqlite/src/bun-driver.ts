import type { SQLiteDriver, SQLiteRecord } from "./driver.ts";

/** Bun-only entrypoint. Use a private connection for each database handle. */
export async function bunSQLiteDriver(filename: string): Promise<SQLiteDriver> {
  // Keep this built-in lazy even when a Node bundler follows the Bun branch.
  const { Database } = await import('bun:sqlite');
  const connection = new Database(filename, { strict: true });
  return {
    execute(sql, parameters = []) {
      const statement = connection.prepare(sql);
      try { return statement.all(...parameters) as SQLiteRecord[]; }
      finally { statement.finalize(); }
    },
    close() { connection.close(true); },
  };
}
