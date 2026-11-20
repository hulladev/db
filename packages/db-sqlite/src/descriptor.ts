import type { Database, DatabaseEngine, SchemaDefinition, ConnectedDatabase, SourceConnection } from "@hulla/db";
import { createDatabase, registerDatabaseFactory, type DatabaseOptions } from "@hulla/db/engine";
import type { FunctionSource } from "@hulla/db";
import { DatabaseError, type SourcesOf } from "@hulla/db/internal/engine";
import type { SQLiteDriver } from "./driver.ts";
interface SQLiteSettings {
  readonly randomBytes?: (length: number) => Uint8Array;
  readonly transactionTimeoutMs?: number;
  /** Override which driver errors get one startup retry, or disable it. */
  readonly startupRetry?: false | ((error: unknown) => boolean);
}
/** Each instance owns a fresh dedicated driver connection. */
export type BrowserSQLiteConnection = SQLiteSettings & { readonly open: () => SQLiteDriver | PromiseLike<SQLiteDriver>; readonly filename?: never };
export type SQLiteConnection = BrowserSQLiteConnection | (SQLiteSettings & { readonly filename: string; readonly open?: never });
export type Connector<C extends SQLiteConnection = SQLiteConnection> = <S extends SchemaDefinition>(schema: S, settings: C, initialize: boolean) => Promise<ConnectedDatabase<S>>;

function isLockResult(code: unknown): boolean {
  if (typeof code !== "number" || !Number.isInteger(code) || code < 0 || code > 0x7fff_ffff) return false;
  const primary = code & 0xff;
  return primary === 5 || primary === 6;
}

function isLockError(error: unknown): boolean {
  if (!(error instanceof Error)) return false;
  const { code, errcode, errno, resultCode } = error as Error & { code?: unknown; errcode?: unknown; errno?: unknown; resultCode?: unknown };
  if (typeof code === "string" && /^SQLITE_(?:BUSY|LOCKED)(?:_|$)/.test(code)) return true;
  if (isLockResult(errcode ?? errno ?? resultCode)) return true;
  // Expo reports SQLite's result code in the message of its native error.
  if (code !== "ERR_INTERNAL_SQLITE_ERROR") return false;
  const result = /\bError code (\d+)\b/.exec(error.message);
  return result !== null && isLockResult(Number(result[1]));
}

export function sqliteDescriptor<C extends SQLiteConnection = SQLiteConnection>(connect: Connector<C>): DatabaseEngine<"sqlite", C> {
  const open = <S extends SchemaDefinition>(schema: S, settings: C, initialize: boolean) => {
    const policy = settings?.startupRetry;
    if (policy !== undefined && policy !== false && typeof policy !== "function") throw new TypeError("SQLite startupRetry must be false or a function");
    return connect(schema, settings, initialize);
  };
  return Object.freeze({
    name: "sqlite", replicaStorage: true, serialTransactions: true,
    connect: <S extends SchemaDefinition>(schema: S, settings: C) => open(schema, settings, false),
    initialize: <S extends SchemaDefinition>(schema: S, settings: C) => open(schema, settings, true),
    retryStartup(error: unknown, settings: C) {
      if (error instanceof DatabaseError || settings?.startupRetry === false) return false;
      return typeof settings?.startupRetry === "function" ? settings.startupRetry(error) === true : isLockError(error);
    },
  });
}
export type SQLiteConstructor<C extends SQLiteConnection = SQLiteConnection> = <const S extends SchemaDefinition, const F extends FunctionSource | undefined = undefined, const R extends readonly SourceConnection<SourcesOf<S>>[] = readonly SourceConnection<SourcesOf<S>>[]>(options: DatabaseOptions<S, C, F, R>) => Database<S, F>;
export type SQLiteFactory<C extends SQLiteConnection = SQLiteConnection> = SQLiteConstructor<C> & { readonly initialize: SQLiteConstructor<C> };
export function sqliteFactory<C extends SQLiteConnection = SQLiteConnection>(connect: Connector<C>): SQLiteFactory<C> {
  const engine = sqliteDescriptor(connect);
  const create = <const S extends SchemaDefinition, const F extends FunctionSource | undefined = undefined, const R extends readonly SourceConnection<SourcesOf<S>>[] = readonly SourceConnection<SourcesOf<S>>[]>(options: DatabaseOptions<S, C, F, R>) => createDatabase<S, F>(engine, options);
  const initialize = <const S extends SchemaDefinition, const F extends FunctionSource | undefined = undefined, const R extends readonly SourceConnection<SourcesOf<S>>[] = readonly SourceConnection<SourcesOf<S>>[]>(options: DatabaseOptions<S, C, F, R>) => createDatabase<S, F>(engine, options, true);
  registerDatabaseFactory(create, engine);
  registerDatabaseFactory(initialize, engine);
  return Object.assign(create, { initialize });
}
