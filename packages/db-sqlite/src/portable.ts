import { sqliteFactory, type BrowserSQLiteConnection } from "./descriptor.ts";
import { openSQLiteDatabase } from "./runtime.ts";

export function portableSQLite(resolveRandomBytes?: () => Promise<NonNullable<BrowserSQLiteConnection["randomBytes"]>>) {
  return sqliteFactory<BrowserSQLiteConnection>(async (schema, settings, initialize) => {
    if (!settings || typeof settings !== "object" || Reflect.ownKeys(settings).some(key => !["open", "randomBytes", "transactionTimeoutMs", "startupRetry"].includes(key as string))
      || settings.filename !== undefined || typeof settings.open !== "function") throw new TypeError("Browser SQLite requires open: () => a dedicated SQLite driver");
    const options = resolveRandomBytes && settings.randomBytes === undefined && typeof globalThis.crypto?.getRandomValues !== "function"
      ? { ...settings, randomBytes: await resolveRandomBytes() }
      : settings;
    return openSQLiteDatabase(schema, await settings.open(), options, initialize);
  });
}
