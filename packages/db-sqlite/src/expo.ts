/// <reference path="./expo-modules.d.ts" />
import {
  createSQLiteAdapter, isSQLiteLockResult,
  type SQLiteConnection, type SQLiteDriver, type SQLiteFactory, type SQLiteRecord, type SQLiteValue,
} from "./portable.ts";

export type { SQLiteConnection, SQLiteDriver } from "./portable.ts";

export interface ExpoSQLiteConnection {
  getAllAsync<T>(sql: string, parameters: SQLiteValue[]): Promise<T[]>;
  closeAsync(): Promise<void>;
}

/** Transfer a dedicated native connection to the SQLite instance. */
export function expoSQLiteDriver(connection: ExpoSQLiteConnection): SQLiteDriver {
  return {
    execute: (sql, parameters = []) => connection.getAllAsync<SQLiteRecord>(sql, [...parameters]),
    close: () => connection.closeAsync(),
  };
}

async function secureRandomBytes(): Promise<NonNullable<SQLiteConnection["randomBytes"]>> {
  try {
    const { getRandomValues } = await import("expo-crypto");
    if (typeof getRandomValues !== "function") throw new TypeError("expo-crypto requires getRandomValues");
    return length => getRandomValues(new Uint8Array(length));
  } catch (cause) {
    return () => {
      throw new TypeError("Install expo-crypto for secure SQLite UUID generation, or supply randomBytes", { cause });
    };
  }
}

async function openDatabase(filename: string): Promise<SQLiteDriver> {
  const { openDatabaseAsync } = await import("expo-sqlite");
  return expoSQLiteDriver(await openDatabaseAsync(filename, { useNewConnection: true }));
}

function isExpoLockError(error: unknown): boolean {
  // Expo reports the SQLite result code in the native error message.
  if (!(error instanceof Error) || !("code" in error) || error.code !== "ERR_INTERNAL_SQLITE_ERROR") return false;
  const result = /\bError code (\d+)\b/.exec(error.message);
  return result !== null && isSQLiteLockResult(Number(result[1]));
}

export const sqlite: SQLiteFactory = createSQLiteAdapter<SQLiteConnection>(async settings => {
  if (!settings || typeof settings !== "object" || Reflect.ownKeys(settings).some(key =>
    !["filename", "open", "randomBytes", "transactionTimeoutMs", "startupRetry"].includes(key as string))
    || (settings.filename === undefined) === (settings.open === undefined)) {
    throw new TypeError("Expo SQLite requires exactly one of filename or open");
  }
  let open = settings.open;
  const { filename } = settings;
  if (filename !== undefined) {
    if (typeof filename !== "string" || !filename || filename.includes("\0")) throw new TypeError("SQLite filename must be a nonempty string");
    open = () => openDatabase(filename);
  }
  if (typeof open !== "function") throw new TypeError("SQLite open must create a dedicated connection");
  const randomBytes = settings.randomBytes ?? (typeof globalThis.crypto?.getRandomValues === "function"
    ? undefined
    : await secureRandomBytes());
  return { ...settings, open, ...(randomBytes === undefined ? {} : { randomBytes }) };
}, isExpoLockError);
