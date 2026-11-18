import { basename, dirname, extname, resolve, relative, sep } from "node:path";
import { discoverFunctions, type GenerationOptions } from "./codegen.ts";
import type { DatabaseConfiguration } from "./database-configuration.ts";
import { FunctionAnalysis, type DatabaseDeclaration } from "./function-analysis.ts";
import { isFunctionDefinition } from "./declarations.ts";
import { functionDatabases } from "./database-metadata.ts";
import { assertModuleName } from "./names.ts";
import { compileWorker, type FunctionImportCheck } from "./worker-codegen.ts";

export function catalogueImports(analysis: FunctionAnalysis, declarations: readonly DatabaseDeclaration[] = []): FunctionImportCheck {
  const known = new Set(declarations.flatMap(value => value.functions?.kind === "import" ? [JSON.stringify([resolve(value.functions.importer), value.functions.specifier])] : []));
  return async (file, specifier) => {
    if (known.has(JSON.stringify([file, specifier]))) return true;
    const values = await analysis.databaseDeclarations(file);
    return values.some(value => value.functions?.kind === "import" && resolve(value.functions.importer) === file && value.functions.specifier === specifier);
  };
}

/** Resolve explicit selectors before executing selected declaration modules. */
export async function databaseFunctions(database: DatabaseConfiguration, source: string, outDir: string, declarations: readonly DatabaseDeclaration[], options: Pick<GenerationOptions, "functions" | "moduleRoot">, analysis: FunctionAnalysis): Promise<Readonly<Record<string, string>>> {
  if (database.functions !== undefined && options.functions !== undefined) throw new TypeError("Declare functions in database initialization or hosting config, not both");
  const selection = declarations.find(value => value.functions)?.functions;
  let modules: Readonly<Record<string, string>>;
  if (database.functions === undefined) {
    // An omitted catalogue means CRUD only. Legacy hosting may still opt into
    // its explicit discovery directory; never expose nearby handlers implicitly.
    modules = options.functions === undefined ? {} : await discoverFunctions({ ...options, schema: source, outDir }, analysis);
  } else if (!selection) throw new TypeError("Cannot resolve functions configuration; declare it in the exported database constructor options");
  else if (selection.kind === "import") {
    const path = Bun.resolveSync(selection.specifier, dirname(selection.importer));
    await analysis.open([path]);
    const name = basename(path, extname(path));
    modules = await analysis.catalogue(path, name === "index" ? basename(dirname(path)) : name);
    if (!Object.keys(modules).length) throw new TypeError("The functions import exports no query or mutation declarations");
  } else {
    const root = resolve(options.moduleRoot ?? dirname(selection.file));
    const selected: Record<string, string> = Object.create(null);
    for await (const path of new Bun.Glob(selection.pattern).scan({ cwd: dirname(selection.file), absolute: true, onlyFiles: true, followSymlinks: false })) {
      const parts = relative(root, path).split(sep);
      if (parts.some(part => part.startsWith(".") || ["_generated", "node_modules", "tests", "test", "__tests__", "__mocks__"].includes(part)) || !/\.[cm]?[tj]sx?$/.test(path) || /\.(?:d|test|spec)\.[cm]?[tj]sx?$/.test(path) || resolve(path) === source) continue;
      const module = parts.join("/").replace(/\.[cm]?[tj]sx?$/, "");
      assertModuleName(module);
      if (selected[module]) throw new TypeError(`Duplicate function module matched by glob: ${module}`);
      selected[module] = path;
    }
    if (!Object.keys(selected).length) throw new TypeError(`Function glob matched no declaration files: ${selection.pattern}`);
    await analysis.open(Object.values(selected));
    const declarations = await Promise.all(Object.entries(selected).map(async ([name, path]) => [name, (await analysis.functions(path)).length ? path : undefined] as const));
    modules = Object.fromEntries(declarations.filter((entry): entry is readonly [string, string] => entry[1] !== undefined));
    if (!Object.keys(modules).length) throw new TypeError(`Function glob matched no query or mutation declarations: ${selection.pattern}`);
  }
  const selected: Record<string, string> = Object.create(null);
  const allowed = catalogueImports(analysis, declarations);
  // Validate the selected graph once before evaluating any declaration modules.
  // Namespace exports retain handler bodies and their transitive imports.
  const paths = [...new Set(Object.values(modules))];
  if (paths.length) await compileWorker(source, paths.map((path, index) => `export * as module${index} from ${JSON.stringify(path)}`).join('\n'), allowed);
  for (const [name, path] of Object.entries(modules)) {
    assertModuleName(name);
    const values: Record<string, unknown> = await import(Bun.pathToFileURL(path).href);
    const functions = Object.values(values).filter(isFunctionDefinition);
    if (database.functions !== undefined && functions.some(value => functionDatabases.get(value) !== database.identity)) throw new TypeError(`Function catalogue contains declarations owned by another schema: ${name}`);
    if (functions.some(value => functionDatabases.get(value) === database.identity)) selected[name] = path;
  }
  return selected;
}
