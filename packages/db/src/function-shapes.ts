import { API, TypeFlags, SymbolFlags, type Checker, type Type } from "typescript/unstable/async";
import type { ValueShape } from "./codec.ts";

const canonicalPath = (path: string) => {
  const normalized = path.replaceAll("\\", "/");
  return process.platform === "win32" || process.platform === "darwin" ? normalized.toLowerCase() : normalized;
};
const pageSource = canonicalPath(Bun.fileURLToPath(new URL("./page.ts", import.meta.url)));
const pageDeclaration = canonicalPath(Bun.fileURLToPath(new URL("./page.d.ts", import.meta.url)));
async function pageBrand(checker: Checker, type: Type): Promise<string | undefined> {
  const fields = await checker.getPropertiesOfType(type);
  if (fields.length !== 3 || !fields.some(field => field.name === "page") || !fields.some(field => field.name === "cursor")) return;
  const brand = fields.find(field => field.name.startsWith("__@pageEligibility@") && field.declarations.some(declaration => declaration.path === pageSource || declaration.path === pageDeclaration));
  return brand?.name;
}

async function shape(checker: Checker, type: Type | undefined, parents = new Set<number>()): Promise<ValueShape> {
  if (!type || type.isErrorType()) throw new TypeError("Cannot infer a function value contract");
  const alias = await type.getAliasSymbol();
  if (alias?.name === "JsonValue" && alias.declarations.some(declaration => ["json-value.ts", "json-value.d.ts"].some(file => canonicalPath(declaration.path) === canonicalPath(Bun.fileURLToPath(new URL(`./${file}`, import.meta.url)))))) return { kind: "json" };
  if (parents.has(type.id)) throw new TypeError("Recursive function value types are not supported");
  const nested = new Set(parents).add(type.id);
  if (type.flags & (TypeFlags.Any | TypeFlags.Unknown | TypeFlags.Never | TypeFlags.TypeParameter)) {
    const name = type.flags & TypeFlags.Any ? "any" : type.flags & TypeFlags.Unknown ? "unknown" : type.flags & TypeFlags.Never ? "never" : "type parameter";
    throw new TypeError(`Unsupported function value type: ${name}`);
  }
  if (type.flags & TypeFlags.Void || type.flags & TypeFlags.Undefined) return { kind: "undefined" };
  if (type.flags & TypeFlags.Null) return { kind: "null" };
  if (type.isBigIntLiteralType()) throw new TypeError("Bigint literal signatures are not supported; use bigint");
  if (type.flags & TypeFlags.BigInt) return { kind: "bigint" };
  if (type.isLiteralType()) return { kind: "literal", value: type.value as string | number | boolean };
  if (type.flags & TypeFlags.String) return { kind: "string" };
  if (type.flags & TypeFlags.Number) return { kind: "number" };
  if (type.flags & TypeFlags.Boolean) return { kind: "boolean" };
  const symbol = await type.getSymbol();
  if (symbol?.name === "Date" && symbol.declarations.some(declaration => /\/node_modules\/(?:typescript|@typescript\/typescript-[^/]+)\/lib\/lib\.[^/]+\.d\.ts$/i.test(canonicalPath(declaration.path)))) return { kind: "date" };
  if (type.isUnionType()) return { kind: "union", choices: await Promise.all((await type.getTypes()).map(t => shape(checker, t, nested))) };
  if (type.isIntersectionType()) {
    // Phantom ID brands do not add wire fields to a primitive.
    const parts = await type.getTypes();
    const primitive = parts.find(t => t.flags & (TypeFlags.StringLike | TypeFlags.NumberLike | TypeFlags.BigIntLike | TypeFlags.BooleanLike));
    if (primitive) return shape(checker, primitive, nested);
    // Standard Schema extension inputs intersect the original submitted shapes.
    // Ask the checker for merged properties so overlapping fields retain their
    // actual intersection type and required/optional status.
    if (parts.some(part => !(part.flags & TypeFlags.Object) && !part.isIntersectionType())) {
      throw new TypeError("Unsupported intersection value; use portable object fields");
    }
  }
  if (await checker.isArrayType(type)) {
    if (!type.isTypeReference()) throw new TypeError("Unsupported array value");
    return { kind: "array", item: await shape(checker, (await checker.getTypeArguments(type))[0], nested) };
  }
  if (await checker.isTupleType(type)) {
    if (!type.isTypeReference()) throw new TypeError("Unsupported tuple value");
    const target = await type.getTarget();
    if (!target.isTupleType() || target.elementFlags.some(flag => flag !== 1)) throw new TypeError("Only fixed required tuples are supported");
    return { kind: "tuple", items: await Promise.all((await checker.getTypeArguments(type)).map(t => shape(checker, t, nested))) };
  }
  if (type.flags & TypeFlags.Object || type.isIntersectionType()) {
    if ((await checker.getSignaturesOfType(type, 0)).length || (await checker.getIndexInfosOfType(type)).length) {
      throw new TypeError("Functions and open index signatures are not portable function values");
    }
    const symbol = await type.getSymbol();
    if (symbol && ["Date", "Map", "Set", "Promise", "Uint8Array"].includes(symbol.name)) throw new TypeError(`Unsupported function value: ${symbol.name}`);
    const fields: Record<string, { shape: ValueShape; optional: boolean }> = Object.create(null);
    const brand = await pageBrand(checker, type);
    for (const field of await checker.getPropertiesOfType(type)) {
      if (field.name === brand) continue;
      if (field.name.startsWith("__@")) throw new TypeError("Symbol properties are not portable values");
      fields[field.name] = { shape: await shape(checker, await checker.getTypeOfSymbol(field), nested), optional: !!(field.flags & SymbolFlags.Optional) };
    }
    return { kind: "object", fields };
  }
  throw new TypeError(`Unsupported function value type: ${await checker.typeToString(type)}`);
}

/** TypeScript's checker provides inferred contracts; applications need no second result schema. */
export async function compileFunctionShapes(modules: Readonly<Record<string, string>>, declarations: Readonly<Record<string, readonly string[]>>): Promise<Map<string, { input: ValueShape; result: ValueShape; paginated?: true }>> {
  const api = new API();
  try {
    const snapshot = await api.updateSnapshot({ openFiles: Object.values(modules) });
    try {
      const shapes = new Map<string, { input: ValueShape; result: ValueShape; paginated?: true }>();
      for (const [module, source] of Object.entries(modules)) {
        const project = await snapshot.getDefaultProjectForFile(source);
        if (!project) throw new TypeError(`No TypeScript project for ${source}`);
        const file = await project.program.getSourceFile(source);
        if (!file) throw new TypeError(`Cannot read ${source}`);
        const checker = project.checker;
        const symbol = await checker.getSymbolAtLocation(file);
        if (!symbol) throw new TypeError(`No module exports in ${source}`);
        const names = new Set(declarations[module]);
        for (const exported of await checker.getExportsOfModule(symbol)) {
          if (!names.has(exported.name)) continue;
          const definition = await checker.getTypeOfSymbol(exported);
          if (!definition) continue;
          const types = await checker.getPropertyOfType(definition, "$types");
          if (!types) continue;
          let metadata = await checker.getTypeOfSymbol(types);
          if (metadata?.isUnionType()) metadata = (await metadata.getTypes()).find(type => type.flags & TypeFlags.Object);
          if (!metadata) throw new TypeError(`Cannot infer ${module}.${exported.name}`);
          const input = await checker.getPropertyOfType(metadata, "input");
          const output = await checker.getPropertyOfType(metadata, "output");
          if (!input || !output) throw new TypeError("Missing function signature metadata");
          const outputType = await checker.getTypeOfSymbol(output);
          const paginated = outputType && await pageBrand(checker, outputType);
          shapes.set(`${module}.${exported.name}`, {
            ...(paginated ? { paginated: true as const } : {}),
            input: await shape(checker, await checker.getTypeOfSymbol(input)),
            result: await shape(checker, outputType),
          });
        }
      }
      return shapes;
    } finally { await snapshot.dispose(); }
  } finally { await api.close(); }
}
