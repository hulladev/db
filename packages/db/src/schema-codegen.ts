import type { JsonShape } from "./json-value.ts";
import { lstat, mkdir } from "node:fs/promises";
import { dirname, isAbsolute, relative, resolve, sep } from "node:path";
import { validExportName } from "./names.ts";
import {
  compileSchema,
  type KeyGeneration,
  type Scalar,
  type SchemaDefinition,
} from "./schema.ts";

const header = "// Generated schema by @hulla/db. Do not edit.\n";
const scalarNames: Record<Scalar, string> = {
  string: "String",
  boolean: "Boolean",
  int64: "Int64",
  uuid: "Uuid",
  float64: "Float64",
  json: "Json",
  date: "Date",
  timestamp: "Timestamp",
  localTimestamp: "LocalTimestamp",
};

function rustString(value: string): string {
  if (/[\uD800-\uDFFF]/u.test(value))
    throw new TypeError(
      "Rust schema strings must contain valid Unicode scalars",
    );
  return `"${[...value]
    .map((char) => {
      if (char === '"' || char === "\\") return `\\${char}`;
      const code = char.codePointAt(0)!;
      return code < 32 || code === 127 ? `\\u{${code.toString(16)}}` : char;
    })
    .join("")}"`;
}

function rustValue(scalar: Scalar, value: unknown): string {
  if (value === null) return "hulladb_schema::Value::Null";
  if (scalar === "json") return `hulladb_schema::Value::json(${rustString(JSON.stringify(value))}).expect("validated JSON default")`;
  if (scalar === "float64") return `hulladb_schema::Value::float64(${value}f64).expect("finite number default")`;
  if (["date", "timestamp", "localTimestamp"].includes(scalar)) return `hulladb_schema::Value::${scalarNames[scalar]}(${rustString(value as string)}.into())`;
  if (scalar === "string")
    return `hulladb_schema::Value::String(${rustString(value as string)}.into())`;
  if (scalar === "boolean") return `hulladb_schema::Value::Boolean(${value})`;
  if (scalar === "int64") return `hulladb_schema::Value::Int64(${value}i64)`;
  const bytes = (value as string).replaceAll("-", "").match(/../g)!;
  return `hulladb_schema::Value::Uuid([${bytes.map((byte) => `0x${byte}`).join(", ")}])`;
}

function rustJsonShape(shape: JsonShape): string {
  const prefix = "hulladb_schema::JsonShape::";
  switch (shape.kind) {
    case "any": return prefix + "Any";
    case "string": return prefix + "String";
    case "boolean": return prefix + "Boolean";
    case "number": return prefix + "Number";
    case "array": return `${prefix}Array { item: Box::new(${rustJsonShape(shape.item)}) }`;
    case "nullable": return `${prefix}Nullable { inner: Box::new(${rustJsonShape(shape.inner)}) }`;
    case "object": return `${prefix}Object { fields: [${Object.entries(shape.fields).map(([key, field]) => `(${rustString(key)}.into(), hulladb_schema::JsonProperty { shape: ${rustJsonShape(field.shape)}, optional: ${field.optional} })`).join(", ")}].into() }`;
  }
}

function rustGenerator(generation: KeyGeneration): string {
  if (typeof generation === "object")
    return `hulladb_schema::KeyGeneration::Custom { handler: ${rustString(generation.handler)}.into(), output_type: hulladb_schema::ScalarType::${scalarNames[generation.outputType]} }`;
  return `hulladb_schema::KeyGeneration::${generation === "none" ? "None" : generation === "uuidV4" ? "UuidV4" : "UuidV7"}`;
}

/** Emit native declarations, then let Rust normalize through its existing contract. */
export function generateRustSchema(schema: SchemaDefinition): string {
  const tables = compileSchema(schema);
  const lines = [
    header.trimEnd(),
    "",
    "pub fn schema() -> Result<hulladb_schema::SchemaContract, hulladb_schema::SchemaError> {",
    "    let tables = vec![",
  ];
  for (const table of tables) {
    lines.push(
      "        {",
      `            let mut table = hulladb_schema::TableDeclaration::new(${rustString(table.name)}, vec![`,
    );
    for (const [name, field] of Object.entries(table.fields)) {
      const boundScalar = field.scalar === "string" ? "int64" : field.scalar;
      const minimum = field.constraints?.min;
      const maximum = field.constraints?.max;
      const constraints = `hulladb_schema::FieldConstraints { min: ${minimum === undefined ? "None" : `Some(${rustValue(boundScalar, minimum)})`}, max: ${maximum === undefined ? "None" : `Some(${rustValue(boundScalar, maximum)})`}, one_of: vec![${(field.constraints?.oneOf ?? []).map(value => rustValue(field.scalar, value)).join(", ")}] }`;
      lines.push(
        `                hulladb_schema::FieldDefinition { name: ${rustString(name)}.into(), scalar_type: hulladb_schema::ScalarType::${scalarNames[field.scalar]}, nullable: ${field.allowsNull}, precision: ${field.precision === undefined ? "None" : `Some(${field.precision})`}, json_shape: ${field.jsonShape ? `Some(${rustJsonShape(field.jsonShape)})` : "None"}, default: ${field.hasDefault ? `Some(${rustValue(field.scalar, field.encode(field.defaultValue))})` : "None"}, constraints: ${constraints} },`,
      );
    }
    lines.push(
      "            ]);",
      `            table.primary_key = Some(${rustString(table.primaryKey)}.into());`,
      `            table.key_generation = Some(${rustGenerator(table.keyGeneration)});`,
      `            table.insert_key_policy = hulladb_schema::InsertKeyPolicy::${table.insertKeyPolicy === "generatedOnly" ? "GeneratedOnly" : "AllowExplicit"};`,
    );
    for (const [name, constraint] of Object.entries(table.unique)) {
      lines.push(
        `            table.unique_constraints.push(hulladb_schema::UniqueConstraintDeclaration { name: ${rustString(name)}.into(), fields: vec![${constraint.fields.map((field) => `${rustString(field)}.into()`).join(", ")}], null_policy: Some(hulladb_schema::UniqueNullPolicy::${constraint.nulls === "distinct" ? "Distinct" : "NotDistinct"}) });`,
      );
    }
    for (const index of table.indexes) lines.push(
      `            table.indexes.push(hulladb_schema::IndexDeclaration { name: ${rustString(index.name)}.into(), fields: vec![${index.fields.map(field => `${rustString(field)}.into()`).join(", ")}] });`,
    );
    for (const check of table.checks) lines.push(
      `            table.checks.push(hulladb_schema::CheckDefinition { name: ${rustString(check.name)}.into(), parts: vec![${check.parts.map(part => `${rustString(part)}.into()`).join(", ")}], operands: vec![${check.operands.map(operand => "field" in operand ? `hulladb_schema::CheckOperand::Field(${rustString(operand.field)}.into())` : `hulladb_schema::CheckOperand::Value(${rustValue(typeof operand.value === "bigint" ? "int64" : typeof operand.value === "number" ? "float64" : typeof operand.value === "boolean" ? "boolean" : "string", operand.value)})`).join(", ")}] });`,
    );
    lines.push("            table", "        },");
  }
  lines.push(
    "    ];",
    "    let mut schema = hulladb_schema::SchemaDeclaration::new(tables);",
    `    schema.default_uuid_generation = hulladb_schema::UuidGeneration::${schema.defaultUuidGeneration === "v4" ? "V4" : "V7"};`,
  );
  for (const table of tables)
    for (const [name, field] of Object.entries(table.fields)) {
      if (typeof field.target === "string")
        lines.push(
          `    { let mut reference = hulladb_schema::ReferenceDeclaration::new(${rustString(table.name)}, ${rustString(name)}, ${rustString(field.target)}); reference.on_delete = hulladb_schema::ReferenceOnDelete::${{ restrict: "Restrict", cascade: "Cascade", setNull: "SetNull" }[field.onDelete ?? "restrict"]}; schema.references.push(reference); }`,
        );
    }
  lines.push("    schema.normalize()", "}", "");
  return lines.join("\n");
}

export interface SchemaGenerationOptions {
  /** Trusted authored module with a default defineSchema export. Executes at build time. */
  readonly schema: string;
  /** Schema types, declarations and query-facing table references are written here. */
  readonly outDir: string;
  /** Optional config module used only to infer the verified identity type. */
  readonly config?: string;
  /** Optional native Rust fixture output; ordinary projects do not need this. */
  readonly rustOutput?: string;
}

export async function generateSchema(
  options: SchemaGenerationOptions,
): Promise<void> {
  const source = resolve(options.schema);
  const outDir = resolve(options.outDir);
  const fromOutput = relative(outDir, source);
  if (!fromOutput.startsWith(`..${sep}`) && !isAbsolute(fromOutput))
    throw new Error(
      "Schema definitions must be outside the generated output directory",
    );
  const { default: schema } = (await import(Bun.pathToFileURL(source).href)) as {
    default: SchemaDefinition;
  };
  const compiled = compileSchema(schema);
  const rust = options.rustOutput === undefined ? undefined : generateRustSchema(schema);
  let sourceImport = relative(outDir, source).split(sep).join("/");
  if (!sourceImport.startsWith(".")) sourceImport = `./${sourceImport}`;
  const referenceTargets = Object.fromEntries(compiled.map(table => [table.name, Object.fromEntries(Object.entries(table.fields).flatMap(([name, field]) => field.target === undefined ? [] : [[name, field.target]]))]));
  const types = `${header}import type schema from ${JSON.stringify(sourceImport)}
import type { ResolveReferences, Row as SchemaRow, Insert as SchemaInsert, Patch as SchemaPatch, Id as SchemaId } from '@hulla/db/schema'
import type { ReadDatabase as SchemaReadDatabase, WriteDatabase as SchemaWriteDatabase, Database as SchemaDatabase } from '@hulla/db/server'

export type Schema = ResolveReferences<typeof schema, ${JSON.stringify(referenceTargets)}>
export type Database = SchemaDatabase<Schema>
export type ReadDatabase = SchemaReadDatabase<Schema>
export type WriteDatabase = SchemaWriteDatabase<Schema>
export type Tables = keyof Schema['tables']
export type Row<T extends Tables> = SchemaRow<Schema, T>
export type Insert<T extends Tables> = SchemaInsert<Schema, T>
export type Patch<T extends Tables> = SchemaPatch<Schema, T>
export type Id<T extends Tables> = SchemaId<Schema, T>
`;
  let configImport: string | undefined;
  if (options.config !== undefined) {
    configImport = relative(outDir, resolve(options.config)).split(sep).join("/");
    if (!configImport.startsWith(".")) configImport = `./${configImport}`;
  }
  const identityImports = configImport === undefined ? "" : `import type config from ${JSON.stringify(configImport)}\nimport type { ConfigIdentity } from '@hulla/db/config'\n`;
  const functions = `${header}import type { Schema } from './types'
import type { QueryContext, MutationContext } from '@hulla/db/server'
import { defineFunctions } from '@hulla/db/internal/functions'
${identityImports}
export const { query, mutation } = defineFunctions<QueryContext<Schema${configImport === undefined ? "" : ", ConfigIdentity<typeof config>"}>, MutationContext<Schema${configImport === undefined ? "" : ", ConfigIdentity<typeof config>"}>>()
`;
  const tableMetadata = `JSON.parse(${JSON.stringify(JSON.stringify(Object.fromEntries(compiled.map(table => [table.name, Object.fromEntries(Object.entries(table.fields).map(([name, field]) => [name, { ...(field.jsonShape ? { jsonShape: field.jsonShape } : {}) }]))]))))})`;
  const tableReferences = `${header}import type { Schema } from './types'
import { createTables } from '@hulla/db/schema'

const $tables = createTables<Schema>(${tableMetadata})
export default $tables
${compiled.filter(table => validExportName(table.name)).map((table, index) => `const $table${index} = $tables[${JSON.stringify(table.name)}]\nexport { $table${index} as ${table.name} }`).join("\n")}
`;
  const outputs = new Map([
    [resolve(outDir, "types.ts"), types],
    [resolve(outDir, "functions.ts"), functions],
    [resolve(outDir, "tables.ts"), tableReferences],
  ]);
  if (options.rustOutput !== undefined) outputs.set(resolve(options.rustOutput), rust!);
  if (outputs.size !== (options.rustOutput === undefined ? 3 : 4))
    throw new Error("Schema outputs must use distinct paths");
  // Check every destination before writing; never replace authored code.
  await checkGeneratedDirectory(outDir);
  for (const path of outputs.keys()) {
    await checkGeneratedDirectory(dirname(path));
    const info = await lstat(path).catch((error: NodeJS.ErrnoException) => {
      if (error.code === "ENOENT") return undefined;
      throw error;
    });
    if (info && (!info.isFile() || info.isSymbolicLink()))
      throw new Error(`Refusing to overwrite an authored path: ${path}`);
    let existing: string;
    try {
      existing = await Bun.file(path).text();
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") continue;
      throw error;
    }
    if (existing.startsWith("// Generated by @hulla/db. Do not edit.\n")) throw new Error(`Schema output conflicts with a generated function file: ${path}`);
    if (!existing.startsWith(header))
      throw new Error(`Refusing to overwrite an authored file: ${path}`);
  }
  for (const [path, content] of outputs) {
    await mkdir(dirname(path), { recursive: true });
    await Bun.write(path, content);
  }
}

/** Check the requested directory and missing ancestors before recursive mkdir. */
export async function checkGeneratedDirectory(directory: string): Promise<void> {
  const absolute = resolve(directory);
  const info = await lstat(absolute).catch((error: NodeJS.ErrnoException) => {
    if (error.code === "ENOENT") return undefined;
    throw error;
  });
  if (info) {
    if (!info.isDirectory() || info.isSymbolicLink())
      throw new Error(`Refusing to overwrite an authored path: ${absolute}`);
    return;
  }
  const parent = dirname(absolute);
  if (parent !== absolute) await checkGeneratedDirectory(parent);
}
