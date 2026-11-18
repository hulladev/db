import { databaseConfiguration } from './database-configuration.ts';
import { factoryEngine } from './database-instance.ts';
import type { FunctionAnalysis, DatabaseDeclaration } from './function-analysis.ts';
import type { SchemaDefinition } from './schema.ts';

/** Inspect typed registrations without importing or running application startup code. */
export async function captureDatabaseEntry(source: string, analysis: FunctionAnalysis, declarations?: readonly DatabaseDeclaration[]): Promise<Record<string, unknown> | undefined> {
  const values = declarations ?? await analysis.databaseDeclarations(source);
  if (!values.length) return undefined;
  if ((await analysis.functions(source)).length) throw new TypeError('Keep schema/function declarations separate from database initialization');
  const result: Record<string, unknown> = Object.create(null);
  for (const value of values) {
    if (!value.schemaExport) throw new TypeError('Generation requires an exported schema from a separate inert authoring module');
    if (!value.factoryExport) throw new TypeError('Generation requires an imported database adapter factory');
    const implementation = (file: string) => file.replace(/\.d\.(ts|mts|cts)$/, (_match, extension: string) => extension === 'mts' ? '.mjs' : extension === 'cts' ? '.cjs' : '.js');
    const schemas = await import(Bun.pathToFileURL(implementation(value.schemaExport.file)).href);
    const adapters = await import(Bun.pathToFileURL(implementation(value.factoryExport.file)).href);
    const engine = factoryEngine(adapters[value.factoryExport.name]);
    const functions = value.functions?.kind === 'glob' ? value.functions.pattern : value.functions ? async () => ({}) : undefined;
    result[value.exportName] = databaseConfiguration(engine, { schema: schemas[value.schemaExport.name] as SchemaDefinition, ...(functions === undefined ? {} : { functions }) });
  }
  return result;
}
