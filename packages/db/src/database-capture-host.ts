import { databaseConfiguration } from './database-configuration.ts';
import { factoryEngine } from './database-instance.ts';
import type { SchemaDefinition } from './schema.ts';
import type { FunctionSource } from './function-source.ts';

/** Generated server settings replace only the initializer, without opening storage. */
export function captureHostDatabase(factory: unknown, options: { readonly schema: SchemaDefinition; readonly functions?: FunctionSource | undefined } & object) {
  return databaseConfiguration(factoryEngine(factory), options);
}
