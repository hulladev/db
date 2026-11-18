import type { Database, DatabaseEngine, SchemaDefinition, ConnectedDatabase, SourceConnection } from "@hulla/db";
import { createDatabase, registerDatabaseFactory, type DatabaseOptions } from "@hulla/db/engine";
import type { FunctionSource } from "@hulla/db";
import type { SourcesOf } from "@hulla/db/internal/engine";
import type { SQLiteDriver } from "./driver.ts";
interface SQLiteSettings {
  readonly randomBytes?: (length: number) => Uint8Array;
  readonly transactionTimeoutMs?: number;
}
/** Each instance owns a fresh dedicated driver connection. */
export type BrowserSQLiteConnection = SQLiteSettings & { readonly open: () => SQLiteDriver | PromiseLike<SQLiteDriver>; readonly filename?: never };
export type SQLiteConnection = BrowserSQLiteConnection | (SQLiteSettings & { readonly filename: string; readonly open?: never });
export type Connector<C extends SQLiteConnection = SQLiteConnection> = <S extends SchemaDefinition>(schema: S, settings: C, initialize: boolean) => Promise<ConnectedDatabase<S>>;
export function sqliteDescriptor<C extends SQLiteConnection = SQLiteConnection>(connect: Connector<C>): DatabaseEngine<"sqlite", C> {
  return Object.freeze({
    name: "sqlite", replicaStorage: true, serialTransactions: true,
    connect: <S extends SchemaDefinition>(schema: S, settings: C) => connect(schema, settings, false),
    initialize: <S extends SchemaDefinition>(schema: S, settings: C) => connect(schema, settings, true),
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
