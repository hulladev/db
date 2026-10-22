import type { DatabaseOperation } from "./database.ts";
import type { CompiledTable } from "./schema.ts";
import { fieldPath, type FieldReference } from "./fields.ts";
import { predicateParser, type Expressions, type Predicate } from "./filters.ts";

const aggregateValue = Symbol("Hulla aggregate value");
export interface AggregateValue<V, Decimal extends boolean = false> {
  readonly [aggregateValue]: { readonly value: V; readonly decimal: Decimal };
}
type Keys<R, V> = { [K in Extract<keyof R, string>]: NonNullable<R[K]> extends V ? K : never }[Extract<keyof R, string>];
type AggregateField<R, V, T extends string> = Keys<R, V> | { [K in Keys<R, V>]: FieldReference<R[K], T> }[Keys<R, V>];
type Value<R, F> = F extends FieldReference<unknown, string, infer V> ? V : F extends keyof R ? R[F] : never;
type Average<V> = V extends bigint ? string : V extends number ? number : never;
type BaseField<R, B, V, T extends string> = AggregateField<B, V, T> | { [K in Keys<B, V> & keyof R]: FieldReference<R[K], T, B[K]> }[Keys<B, V> & keyof R];
export interface Aggregates<R, T extends string = string, B = R> {
  count(): AggregateValue<number>;
  sum<F extends BaseField<R, B, number | bigint, T>>(field: F): AggregateValue<NonNullable<Value<B, F>> | null>;
  avg<F extends BaseField<R, B, number | bigint, T>>(field: F): AggregateValue<Average<Value<B, F>> | null, Extract<Value<B, F>, bigint> extends never ? false : true>;
  min<F extends BaseField<R, B, string | number | bigint | Date, T>>(field: F): AggregateValue<NonNullable<Value<B, F>> | null>;
  max<F extends BaseField<R, B, string | number | bigint | Date, T>>(field: F): AggregateValue<NonNullable<Value<B, F>> | null>;
}
export type AggregateSelection = Record<string, AggregateValue<unknown, boolean>>;
export type AggregateResult<A extends AggregateSelection> = { [K in keyof A]: A[K] extends AggregateValue<infer V, boolean> ? V : never };
type DecimalKeys<A extends AggregateSelection> = { [K in keyof A]: A[K] extends AggregateValue<unknown, true> ? K : never }[keyof A];
type TextHelpers = "contains" | "startsWith" | "endsWith" | "like" | "ilike";
type HavingExpressions<R, T extends string, N extends keyof R, B = R> =
  Omit<Expressions<R, T, B>, TextHelpers> & Pick<Expressions<Omit<R, N>, T, Omit<B, N>>, TextHelpers>;
export interface GroupedAggregate<R, T extends string = string, N extends keyof R = never, B = R> extends DatabaseOperation<R[]> {
  having(predicate: (expressions: HavingExpressions<R, T, N, B>) => Predicate): GroupedAggregate<R, T, N, B>;
  orderBy(field: Extract<keyof R, string>, direction?: "asc" | "desc"): GroupedAggregate<R, T, N, B>;
  limit(count: number): DatabaseOperation<R[]>;
}
export interface GroupedQuery<R, G extends keyof R, T extends string = string, B = R> {
  aggregate<A extends AggregateSelection>(
    select: (aggregates: Aggregates<R, T, B>) => A & (Extract<keyof A, G> extends never ? unknown : { readonly duplicateGroupKeys: never }),
  ): GroupedAggregate<Pick<R, G> & AggregateResult<A>, T, Extract<DecimalKeys<A>, keyof (Pick<R, G> & AggregateResult<A>)>, Pick<B, G & keyof B> & AggregateResult<A>>;
}
export type GroupKey<R> = Keys<R, string | number | bigint | boolean | Date>;

export type AggregateDescriptor = { op: "count" | "sum" | "avg" | "min" | "max"; field?: string };
type Descriptor = { scalar: string; parse(value: unknown): unknown };
export type AggregatePlan = {
  groupBy: string[];
  aggregates: Record<string, AggregateDescriptor>;
  parseHaving(args: readonly unknown[]): Predicate[];
  parseOrder(field: unknown): string;
  decode(result: unknown): Record<string, unknown> | Record<string, unknown>[];
};

function integer(value: unknown): bigint {
  if (typeof value !== "string" || !/^(0|-?[1-9][0-9]*)$/.test(value)) throw new TypeError("Invalid aggregate bigint result");
  return BigInt(value);
}
function decimal(value: unknown): string {
  if (typeof value !== "string" || !/^-?(0|[1-9][0-9]*)(\.[0-9]+)?$/.test(value)) throw new TypeError("Expected a decimal string");
  return value;
}
function number(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value)) throw new TypeError("Invalid aggregate number result");
  return value;
}
function count(value: unknown): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) throw new TypeError("Aggregate count must be a nonnegative safe integer");
  return value;
}

/** Aggregate outputs have their own representations: sums are not stored int64 fields. */
export function aggregatePlan(table: CompiledTable, groupInputs: readonly unknown[], select: unknown): AggregatePlan {
  function root(input: unknown): string {
    const path = fieldPath(input, table.name);
    if (typeof path !== "string" && path.length !== 1) throw new TypeError("Aggregates require root columns");
    const name = typeof path === "string" ? path : path[0]!;
    if (!Object.hasOwn(table.fields, name)) throw new TypeError(`Unknown field ${table.name}.${name}`);
    if (table.fields[name]!.scalar === "json") throw new TypeError("JSON aggregation and grouping are not supported");
    return name;
  }
  const groupBy = groupInputs.map(root);
  if (new Set(groupBy).size !== groupBy.length) throw new TypeError("Duplicate grouping field");
  const values = new WeakMap<object, AggregateDescriptor>();
  function expression(op: AggregateDescriptor["op"], inputs: unknown[]): object {
    if (inputs.length !== (op === "count" ? 0 : 1)) throw new TypeError(`Invalid ${op} arguments`);
    const descriptor: AggregateDescriptor = op === "count" ? { op } : { op, field: root(inputs[0]) };
    const scalar = descriptor.field === undefined ? undefined : table.fields[descriptor.field]!.scalar;
    if ((op === "sum" || op === "avg") && scalar !== "int64" && scalar !== "float64") throw new TypeError(`${op} requires a numeric column`);
    if ((op === "min" || op === "max") && !["string", "int64", "float64", "date", "timestamp", "localTimestamp"].includes(scalar!)) throw new TypeError(`${op} requires a string or numeric column`);
    const token = Object.freeze({});
    values.set(token, descriptor);
    return token;
  }
  if (typeof select !== "function") throw new TypeError("Aggregate requires a callback");
  const selected = select(Object.freeze(Object.fromEntries(["count", "sum", "avg", "min", "max"].map(op =>
    [op, (...inputs: unknown[]) => expression(op as AggregateDescriptor["op"], inputs)]))));
  if (!selected || typeof selected !== "object" || Array.isArray(selected) || !Object.keys(selected).length) throw new TypeError("Aggregate callback must return named aggregates");
  const descriptors: Record<string, Descriptor> = Object.create(null);
  const decoders: Record<string, (value: unknown) => unknown> = Object.create(null);
  for (const name of groupBy) {
    const field = table.fields[name]!;
    descriptors[name] = field;
    decoders[name] = value => field.decode(value);
  }
  const aggregates = Object.fromEntries(Object.entries(selected).map(([alias, token]) => {
    if (groupBy.includes(alias)) throw new TypeError(`Aggregate alias ${alias} collides with a grouping field`);
    const descriptor = token && typeof token === "object" ? values.get(token) : undefined;
    if (!descriptor) throw new TypeError(`Invalid aggregate ${alias}`);
    const field = descriptor.field === undefined ? undefined : table.fields[descriptor.field]!;
    let output: Descriptor;
    let decode: (value: unknown) => unknown;
    if (descriptor.op === "count") {
      output = { scalar: "float64", parse: count };
      decode = count;
    } else if (descriptor.op === "avg" && field!.scalar === "int64") {
      const bigint = field!.mode === "bigint";
      output = { scalar: "decimal", parse: bigint ? decimal : value => {
        number(value); return String(value);
      } };
      decode = bigint ? decimal : value => number(Number(decimal(value)));
    } else if (field!.scalar === "int64") {
      const bigint = field!.mode === "bigint";
      const operand = (value: unknown) => {
        if (bigint ? typeof value !== "bigint" : typeof value !== "number" || !Number.isSafeInteger(value)) throw new TypeError("Expected safe integer aggregate operand");
        return bigint ? value : BigInt(value as number);
      };
      const decodeInteger = (value: unknown) => {
        const result = integer(value);
        if (descriptor.op !== "sum" && (result < -(1n << 63n) || result >= 1n << 63n)) throw new TypeError("Invalid aggregate int64");
        if (bigint) return result;
        const numeric = Number(result);
        if (!Number.isSafeInteger(numeric)) throw new TypeError("Aggregate int64 exceeds safe integer range; use mode: bigint");
        return numeric;
      };
      output = { scalar: "int64", parse: operand };
      decode = decodeInteger;
    } else if (field!.scalar === "float64") {
      output = { scalar: "float64", parse: number };
      decode = number;
    } else if (["date", "timestamp", "localTimestamp"].includes(field!.scalar) && !field!.customCodec) {
      output = field!;
      decode = value => field!.decode(value);
    } else {
      output = { scalar: field!.scalar, parse: value => field!.storage(value) };
      decode = value => field!.storage(value);
    }
    descriptors[alias] = output;
    decoders[alias] = value => descriptor.op !== "count" && value === null ? null : decode(value);
    return [alias, descriptor];
  }));
  function row(value: unknown): Record<string, unknown> {
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new TypeError("Invalid aggregate result");
    if (Object.keys(value).length !== Object.keys(decoders).length || Object.keys(decoders).some(key => !Object.hasOwn(value, key))) throw new TypeError("Aggregate result does not match requested fields");
    return Object.fromEntries(Object.entries(decoders).map(([key, decode]) => [key, decode((value as Record<string, unknown>)[key])]));
  }
  return {
    groupBy, aggregates,
    parseHaving: predicateParser(table.name, descriptors),
    parseOrder(field) {
      if (typeof field !== "string" || !Object.hasOwn(descriptors, field)) throw new TypeError("Expected a grouping field or aggregate alias");
      return field;
    },
    decode(result) {
      if (!groupBy.length) return row(result);
      if (!Array.isArray(result)) throw new TypeError("Expected grouped aggregate results");
      return result.map(row);
    },
  };
}
