import { DatabaseSync } from "node:sqlite";
import type { SQLiteDriver, SQLiteRecord } from "./driver.ts";

/** Node-only entrypoint. Opening a file is explicit; importing this module opens nothing. */
export function nodeSQLiteDriver(filename: string): SQLiteDriver {
  const connection = new DatabaseSync(filename, { enableForeignKeyConstraints: true, enableDoubleQuotedStringLiterals: false });
  return {
    execute(sql, parameters = []) {
      return connection.prepare(sql).all(...parameters) as SQLiteRecord[];
    },
    close() { connection.close(); },
  };
}
