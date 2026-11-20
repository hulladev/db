/** Values crossing a SQLite driver boundary. Int64 columns are read as text. */
export type SQLiteValue = string | number | null;
export type SQLiteRecord = Record<string, SQLiteValue>;
/** A dedicated connection, owned by the database until close(). Do not share it with other SQL users. */
export interface SQLiteDriver {
  execute(sql: string, parameters?: readonly SQLiteValue[]): PromiseLike<readonly SQLiteRecord[]> | readonly SQLiteRecord[];
  close(): void | PromiseLike<void>;
}

export interface SQLiteWasmConnection {
  exec(options: { sql: string; bind: SQLiteValue[]; rowMode: "object"; resultRows: SQLiteRecord[] }): unknown;
  close(): unknown;
}
/** Wrap an official sqlite3.oo1.DB/OpfsDb in the thread which owns it (normally a worker). */
export function wasmSQLiteDriver(connection: SQLiteWasmConnection): SQLiteDriver {
  return {
    execute(sql, parameters = []) {
      const resultRows: SQLiteRecord[] = [];
      connection.exec({ sql, bind: [...parameters], rowMode: "object", resultRows });
      return resultRows;
    },
    close() { connection.close(); },
  };
}
