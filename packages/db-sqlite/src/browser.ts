import { portableSQLite } from "./portable.ts";
export { wasmSQLiteDriver } from "./driver.ts";
export type { SQLiteWasmConnection, SQLiteDriver } from "./driver.ts";
export type { BrowserSQLiteConnection as SQLiteConnection } from "./descriptor.ts";

export const sqlite = portableSQLite();
