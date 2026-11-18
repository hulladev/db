import type { DatabaseEngine, ConnectedDatabase, SchemaDefinition, FunctionSource, SourceConnection } from "@hulla/db";
import { createDatabase, registerDatabaseFactory, type DatabaseOptions } from "@hulla/db/engine";
import type { SourcesOf } from "@hulla/db/internal/engine";
export interface PostgresConnection { readonly url?: string | undefined; readonly namespace?: string }
export type Connector = <S extends SchemaDefinition>(schema: S, connection: PostgresConnection, initialize: boolean) => Promise<ConnectedDatabase<S>>;
export function postgresDescriptor(connect: Connector): DatabaseEngine<"postgres", PostgresConnection> {
  return Object.freeze({ name: "postgres", replicaStorage: false,
    connect: <S extends SchemaDefinition>(schema: S, settings: PostgresConnection) => connect(schema, settings, false),
    initialize: <S extends SchemaDefinition>(schema: S, settings: PostgresConnection) => connect(schema, settings, true),
  });
}
export function postgresFactory(connect: Connector) {
  const engine = postgresDescriptor(connect);
  const create = <const S extends SchemaDefinition, const F extends FunctionSource | undefined = undefined, const R extends readonly SourceConnection<SourcesOf<S>>[] = readonly SourceConnection<SourcesOf<S>>[]>(options: DatabaseOptions<S, PostgresConnection, F, R>) => createDatabase<S, F>(engine, options);
  const initialize = <const S extends SchemaDefinition, const F extends FunctionSource | undefined = undefined, const R extends readonly SourceConnection<SourcesOf<S>>[] = readonly SourceConnection<SourcesOf<S>>[]>(options: DatabaseOptions<S, PostgresConnection, F, R>) => createDatabase<S, F>(engine, options, true);
  registerDatabaseFactory(create, engine);
  registerDatabaseFactory(initialize, engine);
  return Object.assign(create, { initialize });
}
