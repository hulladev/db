import { sqliteFactory, type BrowserSQLiteConnection, type SQLiteConnection, type SQLiteFactory } from "./descriptor.ts";
import { openSQLiteDatabase, type SQLiteOptions } from "./runtime.ts";

export { isSQLiteLockResult } from "./descriptor.ts";
export type { SQLiteConnection, SQLiteFactory } from "./descriptor.ts";
export type { SQLiteDriver, SQLiteRecord, SQLiteValue } from "./driver.ts";

type DriverOptions = SQLiteOptions & Pick<BrowserSQLiteConnection, "open">;

/** Use a platform driver with the shared SQLite engine and instance lifecycle. */
export function createSQLiteAdapter<C extends SQLiteConnection>(
  resolve: (settings: C) => DriverOptions | PromiseLike<DriverOptions>,
  isDriverLockError?: (error: unknown) => boolean,
): SQLiteFactory<C> {
  return sqliteFactory<C>(async (schema, settings, initialize) => {
    const options = await resolve(settings);
    return openSQLiteDatabase(schema, await options.open(), options, initialize);
  }, isDriverLockError);
}
