import { createSQLiteAdapter } from "./portable.ts";
import type { BrowserSQLiteConnection } from "./descriptor.ts";
export { wasmSQLiteDriver } from "./driver.ts";
export type { SQLiteWasmConnection, SQLiteDriver } from "./driver.ts";
export type { BrowserSQLiteConnection as SQLiteConnection } from "./descriptor.ts";

export const sqlite = createSQLiteAdapter<BrowserSQLiteConnection>(settings => {
  if (!settings || typeof settings !== "object" || Reflect.ownKeys(settings).some(key => !["open", "randomBytes", "transactionTimeoutMs", "startupRetry"].includes(key as string))
    || settings.filename !== undefined || typeof settings.open !== "function") throw new TypeError("Browser SQLite requires open: () => a dedicated SQLite driver");
  return settings;
});
