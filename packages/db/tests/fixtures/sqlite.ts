import { sqlite } from '@hulla/db-sqlite';
import type { SQLiteConnection } from '@hulla/db-sqlite';
import type { SchemaDefinition, Database, SourceConnection } from '../../src/index.ts';

type Settings = SQLiteConnection & { readonly sources?: readonly SourceConnection[] };
async function open<S extends SchemaDefinition>(schema: S, settings: Settings, initialize: boolean): Promise<Database<S>> {
  const db = (initialize ? sqlite.initialize : sqlite)({ schema, ...settings } as never) as Database<S>;
  try { await db.ready(); return db; }
  catch (error) { await db.close(); throw error; }
}
/** Real driver setup shared by integration fixtures, with owned failure cleanup. */
export const initializeDatabase = <S extends SchemaDefinition>(schema: S, settings: Settings) => open(schema, settings, true);
export const connectDatabase = <S extends SchemaDefinition>(schema: S, settings: Settings) => open(schema, settings, false);
