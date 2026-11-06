import { sqliteFactory } from "./descriptor.ts";
export type { SQLiteConnection } from "./descriptor.ts";
export type { SQLiteDriver, SQLiteValue, SQLiteRecord } from "./driver.ts";

export const sqlite = sqliteFactory(async (schema, settings, initialize) => {
    if (!settings || typeof settings !== "object" || Reflect.ownKeys(settings).some(key => !["filename", "open", "randomBytes", "transactionTimeoutMs"].includes(key as string))
      || (settings.filename === undefined) === (settings.open === undefined)) throw new TypeError("SQLite requires exactly one of filename or open");
    let driver;
    if (settings.open !== undefined) {
      if (typeof settings.open !== "function") throw new TypeError("SQLite open must create a dedicated connection");
      driver = await settings.open();
    } else {
      if (typeof settings.filename !== "string" || !settings.filename || settings.filename.includes("\0")) throw new TypeError("SQLite filename must be a nonempty path");
      if (typeof process === "undefined") throw new TypeError("Use the browser/Expo adapter and provide open");
      driver = await (process.versions.bun
        ? (await import("./bun-driver.ts")).bunSQLiteDriver(settings.filename)
        : (await import("./node-driver.ts")).nodeSQLiteDriver(settings.filename));
    }
    const { openSQLiteDatabase } = await import("./runtime.ts");
    return openSQLiteDatabase(schema, driver, settings, initialize);
});
