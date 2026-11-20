import { builtinModules } from "node:module";
import { runtimeExportConditions } from "./runtime-conditions.ts";
import { dirname, resolve } from "node:path";

export const workerBundleHeader = "// Generated restricted worker by @hulla/db. Do not edit.\n";

const escapePattern = (value: string) => value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const forbiddenImports = new RegExp(`^(?:[a-zA-Z][\\w+.-]*:|bun(?:/|$)|(?:${builtinModules.map(escapePattern).join("|")})$)|\\.(?:node|so|dylib)$`);
function unsupportedImport(path: string, importer: string): Error {
  return new Error(`Unsupported declaration import ${JSON.stringify(path)} in ${importer}. Queries and mutations support bundled computation/validation code; move server I/O to ordinary server code.`);
}

/** Build trusted source into one portable script. The execution engine has no loader. */
export type FunctionImportCheck = (importer: string, specifier: string) => Promise<boolean>;
export async function compileWorker(entrypoint: string, contents?: string, functionImports: ReadonlySet<string> | FunctionImportCheck = new Set()): Promise<string> {
  const build = await Bun.build({
    conditions: ["hulladb-worker", ...runtimeExportConditions()],
    entrypoints: [contents === undefined ? entrypoint : "__hulla_virtual_worker__"], target: "browser", format: "iife", throw: false, metafile: true,
    plugins: [{
      name: "hulla-declaration-capabilities",
      setup(builder) {
        if (contents !== undefined) {
          builder.onResolve({ filter: /^__hulla_virtual_worker__$/ }, () => ({ path: entrypoint, namespace: "hulla-worker" }));
          builder.onLoad({ filter: /.*/, namespace: "hulla-worker" }, () => ({ contents, loader: "ts", resolveDir: dirname(entrypoint) }));
        }
        // Leave ordinary resolution to Bun. A wildcard onResolve callback can
        // corrupt reexported namespace bindings in Bun 1.4, including Zod's.
        builder.onResolve({ filter: forbiddenImports }, args => {
          throw unsupportedImport(args.path, args.importer);
        });
      },
    }],
  });
  if (!build.success) throw new Error(`Cannot bundle database declarations:\n${build.logs.map(log => log.message).join("\n")}`);
  if (build.outputs.length !== 1 || !build.metafile) throw new Error("Database declarations must compile to one self-contained script");
  // Inspect dynamic imports without intercepting library namespace resolution.
  // Bun labels tree-shaken relative imports as external in input metadata, even
  // when nothing is external in the emitted bundle. Check the output below for
  // surviving external imports. The native runtime also denies dynamic imports.
  for (const [importer, input] of Object.entries(build.metafile.inputs)) {
    for (const dependency of input.imports) {
      if (dependency.kind === "dynamic-import") {
        const specifier = dependency.original ?? dependency.path;
        if (typeof functionImports === "function" ? await functionImports(resolve(importer), specifier) : functionImports.has(JSON.stringify([resolve(importer), specifier]))) continue;
        throw unsupportedImport(dependency.original ?? dependency.path, importer);
      }
    }
  }
  if (Object.values(build.metafile.outputs).some(output => output.imports.length !== 0)) {
    throw new Error("Database declarations must compile to one self-contained script");
  }
  return workerBundleHeader + await build.outputs[0]!.text();
}
