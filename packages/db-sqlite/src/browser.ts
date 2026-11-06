import { sqliteFactory } from "./descriptor.ts";
import type { BrowserSQLiteConnection } from "./descriptor.ts";
import { openSQLiteDatabase } from "./runtime.ts";
export { wasmSQLiteDriver } from "./driver.ts";
export type { SQLiteWasmConnection, SQLiteDriver } from "./driver.ts";
export type { BrowserSQLiteConnection as SQLiteConnection } from "./descriptor.ts";

export const sqlite = sqliteFactory<BrowserSQLiteConnection>(async (schema, settings, initialize) => {
    if (!settings || typeof settings !== "object" || Reflect.ownKeys(settings).some(key => !["open", "randomBytes", "transactionTimeoutMs"].includes(key as string))
      || settings.filename !== undefined || typeof settings.open !== "function") throw new TypeError("Browser SQLite requires open: () => a dedicated SQLite driver");
    return openSQLiteDatabase(schema, await settings.open(), settings, initialize);
});
