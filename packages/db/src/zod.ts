import { z } from "zod";
import {
  compiledTableForDefinition,
  jsonValidatorForShape,
  v,
  type JsonShape,
  type TableDefinition,
} from "./schema.ts";

type Narrow<S extends z.ZodType, T> = S & z.ZodType<T, T>;
type Native<T> = 0 extends 1 & T ? z.ZodType<T> : [T] extends [Date] ? Narrow<z.ZodDate, T>
  : [T] extends [string] ? Narrow<z.ZodString, T>
  : [T] extends [number] ? Narrow<z.ZodNumber, T>
  : [T] extends [bigint] ? Narrow<z.ZodBigInt, T>
  : [T] extends [boolean] ? Narrow<z.ZodBoolean, T>
  : [T] extends [readonly (infer Item)[]] ? z.ZodArray<Schema<Item>>
  : z.ZodType<T>;
type NullableSchema<T> = null extends T ? z.ZodNullable<Native<Exclude<T, null>>> : Native<T>;
type Schema<T> = undefined extends T ? z.ZodOptional<NullableSchema<Exclude<T, undefined>>> : NullableSchema<T>;
type Present<T> = { [K in keyof T as [Exclude<T[K], undefined>] extends [never] ? never : K]: T[K] };
type Select<D extends TableDefinition> = Present<D["$inferSelect"]>;
type Insert<D extends TableDefinition> = Present<D["$inferInsert"]>;
type Update<D extends TableDefinition> = Present<D["$inferUpdate"]>;
type FieldSchema<D extends TableDefinition, K, T> = K extends keyof D["fields"]
  ? D["fields"][K]["customCodec"] extends true ? z.ZodType<Exclude<T, null | undefined>> : Native<Exclude<T, null | undefined>>
  : Native<Exclude<T, null | undefined>>;
type Refinements<D extends TableDefinition, T> = { [K in keyof T]?: (schema: FieldSchema<D, K, T[K]>) => z.ZodType };
type NullableWrapped<T, S extends z.ZodType> = null extends T ? z.ZodNullable<S> : S;
type Wrapped<T, S extends z.ZodType> = undefined extends T ? z.ZodOptional<NullableWrapped<T, S>> : NullableWrapped<T, S>;
type Refined<D extends TableDefinition, T, R> = { -readonly [K in keyof T]-?: K extends keyof R
  ? R[K] extends (schema: never) => infer S ? S extends z.ZodType ? Wrapped<T[K], S> : Wrapped<T[K], FieldSchema<D, K, T[K]>> : Wrapped<T[K], FieldSchema<D, K, T[K]>>
  : Wrapped<T[K], FieldSchema<D, K, T[K]>> };

/** Native Zod object for a complete application row, including its primary key. */
export function createSelectSchema<D extends TableDefinition, R extends Refinements<D, Select<D>> = {}>(table: D, refinements?: R & Refinements<D, Select<D>>): z.ZodObject<Refined<D, Select<D>, R>> {
  return tableSchema(table, "select", refinements) as never;
}
/** Defaults and generated keys become optional; no database defaults are applied. */
export function createInsertSchema<D extends TableDefinition, R extends Refinements<D, Insert<D>> = {}>(table: D, refinements?: R & Refinements<D, Insert<D>>): z.ZodObject<Refined<D, Insert<D>, R>> {
  return tableSchema(table, "insert", refinements) as never;
}
/** Partial assignment schema with the immutable primary key excluded. */
export function createUpdateSchema<D extends TableDefinition, R extends Refinements<D, Update<D>> = {}>(table: D, refinements?: R & Refinements<D, Update<D>>): z.ZodObject<Refined<D, Update<D>, R>> {
  return tableSchema(table, "update", refinements) as never;
}

type RuntimeField = TableDefinition["fields"][string];
function valid(parse: (value: unknown) => unknown, value: unknown): boolean {
  try { parse(value); return true; } catch { return false; }
}
function jsonSchema(shape: JsonShape): z.ZodType {
  const validator = jsonValidatorForShape(shape);
  if (validator?.customCodec) return z.custom(value => valid(value => validator.parse(value), value), "Invalid application JSON value");
  switch (shape.kind) {
    case "string": return z.string();
    case "number": return z.number();
    case "boolean": return z.boolean();
    case "nullable": return jsonSchema(shape.inner).nullable();
    case "array": return z.array(jsonSchema(shape.item));
    case "object": return z.strictObject(Object.fromEntries(Object.entries(shape.fields).map(([key, field]) => [key, field.optional ? jsonSchema(field.shape).optional() : jsonSchema(field.shape)])));
    case "any": return z.json();
  }
}
function fieldSchema(field: RuntimeField): z.ZodType {
  let schema: z.ZodType;
  if (field.customCodec) schema = z.custom();
  else switch (field.scalar) {
    case "string": schema = z.string(); break;
    case "uuid": schema = z.string().regex(/^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/); break;
    case "float64": schema = z.number(); break;
    case "int64": schema = field.mode === "bigint" ? z.bigint() : z.number().int().min(Number.MIN_SAFE_INTEGER).max(Number.MAX_SAFE_INTEGER); break;
    case "boolean": schema = z.boolean(); break;
    case "date": case "timestamp": case "localTimestamp": schema = field.mode === "date" ? z.date() : z.string(); break;
    case "json": schema = jsonSchema(field.jsonShape ?? { kind: "any" }); break;
  }
  return schema;
}
function tableSchema(table: TableDefinition, operation: "select" | "insert" | "update", refinements: object = {}): z.ZodObject<z.ZodRawShape> {
  const compiled = compiledTableForDefinition(table);
  if (!compiled && Object.values(table.fields).some(field => typeof field.target === "string")) throw new TypeError("Call defineSchema before adapting a table with string v.id references");
  const key = table.options.primaryKey ?? "id";
  const fields = { ...table.fields, ...(Object.hasOwn(table.fields, key) ? {} : { [key]: v.uuid() }), ...compiled?.fields };
  const generation = table.options.keyGeneration ?? (fields[key]?.scalar === "uuid" ? "uuidV7" : "none");
  const shape: Record<string, z.ZodType> = {};
  for (const [name, field] of Object.entries(fields)) {
    if ((operation === "update" && name === key) || (operation === "insert" && name === key && table.options.insertKeyPolicy === "generatedOnly")) continue;
    let schema = fieldSchema(field);
    const refine = Object.getOwnPropertyDescriptor(refinements, name);
    if (refine) {
      if (!("value" in refine) || typeof refine.value !== "function") throw new TypeError(`Expected a Zod refinement callback for ${name}`);
      schema = refine.value(schema);
      if (!(schema instanceof z.ZodType)) throw new TypeError(`Refinement for ${name} must return a Zod schema`);
    }
    if (field.allowsNull) schema = schema.nullable();
    // Validate storage constraints/codecs without changing the accepted value.
    schema = schema.refine(value => valid(value => field.parse(value), value), "Value violates its database field definition");
    if (operation === "update" || (operation === "insert" && (field.hasDefault || (name === key && generation !== "none")))) schema = schema.optional();
    shape[name] = schema;
  }
  for (const name of Object.keys(refinements)) if (!Object.hasOwn(shape, name)) throw new TypeError(`Cannot refine excluded or unknown field ${name}`);
  return z.strictObject(shape);
}
