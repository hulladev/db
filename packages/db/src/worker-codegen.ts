import { builtinModules } from "node:module";
import { runtimeExportConditions } from "./runtime-conditions.ts";

export const workerBundleHeader = "// Generated restricted worker by @hulla/db. Do not edit.\n";

const escapePattern = (value: string) => value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const forbiddenImports = new RegExp(`^(?:[a-zA-Z][\\w+.-]*:|bun(?:/|$)|(?:${builtinModules.map(escapePattern).join("|")})$)|\\.(?:node|so|dylib)$`);
function unsupportedImport(path: string, importer: string): Error {
  return new Error(`Unsupported declaration import ${JSON.stringify(path)} in ${importer}. Queries and mutations support bundled computation/validation code; move server I/O to ordinary server code.`);
}

/** Build trusted source into one portable script. The execution engine has no loader. */
export async function compileWorker(entrypoint: string): Promise<string> {
  const build = await Bun.build({
    conditions: runtimeExportConditions(),
    entrypoints: [entrypoint], target: "browser", format: "iife", throw: false, metafile: true,
    plugins: [{
      name: "hulla-declaration-capabilities",
      setup(builder) {
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
        throw unsupportedImport(dependency.original ?? dependency.path, importer);
      }
    }
  }
  if (Object.values(build.metafile.outputs).some(output => output.imports.length !== 0)) {
    throw new Error("Database declarations must compile to one self-contained script");
  }
  return workerBundleHeader + await build.outputs[0]!.text();
}
