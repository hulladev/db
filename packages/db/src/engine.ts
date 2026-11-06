import type { SchemaDefinition } from "./schema.ts";
import type { WriteDatabase } from "./database.ts";
import type { SourcesOf } from "./connection-options.ts";
import type { ReplicaConnection } from "./device-connection.ts";
import type { SourceConnection, SourceSettings } from "./connection-options.ts";
import type { FunctionSource } from "./function-source.ts";
export { createDatabase } from "./database-instance.ts";
export { registerDatabaseFactory } from "./database-instance.ts";
export type { Database, DatabaseState, FunctionBindings, SourceFunctions } from "./database-instance.ts";
export type DatabaseOptions<S extends SchemaDefinition, Connection, F extends FunctionSource | undefined, C extends readonly SourceConnection<SourcesOf<S>>[]> =
  Connection & { readonly schema: S; readonly functions?: F } & SourceSettings<S, C>;

export interface TransactionOptions { readonly retries?: number }
export interface ConnectedDatabase<S extends SchemaDefinition> extends WriteDatabase<S> {
  transaction<T>(handler: (tx: WriteDatabase<S>) => T | PromiseLike<T>, options?: TransactionOptions): Promise<T>;
  close(): Promise<void>;
}
export type ConnectedDefinition<S extends SchemaDefinition> = ConnectedDatabase<S> & ([SourcesOf<S>] extends [never] ? {} : ReplicaConnection<SourcesOf<S>>);

/** Low-level adapter contract. Public sqlite/postgres factories own instance startup. */
export interface DatabaseEngine<Name extends string = string, Connection = never> {
  readonly name: Name;
  readonly replicaStorage: boolean;
  /** Dedicated single-connection adapters serialize complete local handler invocations. */
  readonly serialTransactions?: boolean;
  connect<S extends SchemaDefinition>(schema: S, connection: Connection): Promise<ConnectedDatabase<S>>;
  initialize<S extends SchemaDefinition>(schema: S, connection: Connection): Promise<ConnectedDatabase<S>>;
}
