import type { Row, StorageRow, SchemaDefinition } from "./schema.ts";

const reference = Symbol("Hulla table reference");
const field = Symbol("Hulla field reference");
const paths = new WeakMap<object, readonly string[]>();
const valueType = Symbol("Hulla field value type");
const storageType = Symbol("Hulla field storage type");
import type { JsonShape } from "./json-value.ts";

export interface FieldReference<Value, Table extends string = string, Stored = unknown> {
  readonly [field]: { readonly table: Table; readonly name: string };
  readonly [valueType]?: Value;
  readonly [storageType]?: Stored;
}

export type NestedReference<V, T extends string, B = V> = FieldReference<V, T, B> & (NonNullable<B> extends readonly unknown[] | Date ? {} : NonNullable<B> extends object ? { readonly [K in Extract<keyof NonNullable<V>, string>]: NestedReference<NonNullable<V>[K], T, K extends keyof NonNullable<B> ? NonNullable<B>[K] : NonNullable<V>[K]> } : {});

export type TableReference<
  S extends SchemaDefinition,
  T extends Extract<keyof S["tables"], string>,
> = {
  readonly [reference]: T;
} & {
  readonly [K in keyof Row<S, T>]: NestedReference<Row<S, T>[K], T, StorageRow<S, T>[K]>;
};

export function createTables<S extends SchemaDefinition>(
  metadata: Readonly<Record<string, Readonly<Record<string, { jsonShape?: JsonShape }>>>>,
): {
  readonly [T in Extract<keyof S["tables"], string>]: TableReference<S, T>;
} {
  const tables = Object.entries(metadata).map(([name, fields]) => {
    function nested(segments: readonly string[], shape?: JsonShape): object {
      const key = segments.join(".");
      const properties: Record<string | symbol, unknown> = Object.create(null);
      properties[field] = Object.freeze({ table: name, name: key });
      if (shape?.kind === "nullable") shape = shape.inner;
      if (shape?.kind === "object") {
        for (const [child, descriptor] of Object.entries(shape.fields)) {
          properties[child] = nested([...segments, child], descriptor.shape);
        }
      }
      const result = new Proxy(Object.freeze(properties), {
        get(target, property, receiver) {
          if (typeof property === "string" && !Object.hasOwn(target, property) && shape?.kind === "any") return nested([...segments, property], shape);
          return Reflect.get(target, property, receiver);
        },
      });
      paths.set(result, Object.freeze([...segments]));
      return result;
    }
    const properties = Object.entries(fields)
      .map(([key, descriptor]) => [key, nested([key], (descriptor as { jsonShape?: JsonShape }).jsonShape)]);
    const table = Object.assign(
      Object.create(null),
      { [reference]: name },
      Object.fromEntries(properties),
    );
    return [name, Object.freeze(table)];
  });
  return Object.freeze(Object.fromEntries(tables)) as never;
}

export function tableName(value: unknown): string {
  if (typeof value === "string") return value;
  if (typeof value === "object" && value !== null && reference in value) {
    return (value as { [reference]: string })[reference];
  }
  throw new TypeError("Expected a table name or table reference");
}

export function fieldName(value: unknown, table: string): string {
  if (typeof value === "string") return value;
  const metadata = fieldMetadata(value);
  if (!metadata || metadata.table !== table) {
    throw new TypeError("Expected a field from the selected table");
  }
  return metadata.name;
}

export function fieldMetadata(
  value: unknown,
): { table: string; name: string } | undefined {
  return typeof value === "object" && value !== null && field in value
    ? (value as FieldReference<unknown>)[field]
    : undefined;
}

/** Preserve literal JSON keys in generated references; string paths retain dotted syntax. */
export function fieldPath(value: unknown, table: string): string | readonly string[] {
  const name = fieldName(value, table);
  const segments = typeof value === "object" && value !== null ? paths.get(value) : undefined;
  return segments && (segments.length > 1 || segments[0]!.includes(".")) ? segments : name;
}
