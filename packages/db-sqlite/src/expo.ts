/// <reference path="./expo-crypto.d.ts" />
import { portableSQLite } from "./portable.ts";
export { expoSQLiteDriver } from "./driver.ts";
export type { ExpoSQLiteConnection, SQLiteDriver } from "./driver.ts";
export type { BrowserSQLiteConnection as SQLiteConnection } from "./descriptor.ts";

export const sqlite = portableSQLite(async () => {
  try {
    const { getRandomValues } = await import("expo-crypto");
    if (typeof getRandomValues !== "function") throw new TypeError("expo-crypto requires getRandomValues");
    return length => getRandomValues(new Uint8Array(length));
  } catch (cause) {
    return () => {
      throw new TypeError("Install expo-crypto for secure SQLite UUID generation, or supply randomBytes", { cause });
    };
  }
});
