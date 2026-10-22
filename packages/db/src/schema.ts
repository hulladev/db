import { validateJson, type JsonShape, type JsonValue } from "./json-value.ts";
export type { JsonShape, JsonValue } from "./json-value.ts";
import type { Validator } from "./validation.ts";
import { compileChecks, type CheckFields, type SqlCheck, type CompiledCheck } from "./sql-check.ts";
export { sql } from "./sql-check.ts";
export { createTables } from "./fields.ts";
export type { TableReference, FieldReference } from "./fields.ts";

export type Scalar = "string" | "boolean" | "int64" | "uuid" | "float64" | "json" | "date" | "timestamp" | "localTimestamp";
type Nullable<T, N extends boolean> = N extends true ? T | null : T;
const fieldBrand = Symbol("Hulla schema field");
export interface FieldCodec<App, Stored> {
  readonly encode: (value: App) => Stored;
  readonly decode: (value: Stored) => App;
}
type Representation = { encode(value: unknown): unknown; decode(value: unknown): unknown };
const representations = new WeakMap<object, Representation>();
export interface FieldConstraints {
  readonly min?: number | bigint;
  readonly max?: number | bigint;
  readonly oneOf?: readonly (string | number | bigint | boolean)[];
}
export interface TableShape { readonly fields: Fields; readonly options: TableOptions; readonly [tableBrand]: true; readonly indexes: readonly IndexDefinition[] }
export type ReferenceTarget = string | TableShape;
export interface ReferenceOptions { readonly onDelete?: "restrict" | "cascade" | "setNull" }
type FieldOptions = { precision?: number; mode?: "number" | "bigint" | "string" | "date"; customCodec?: boolean; constraints?: FieldConstraints; onDelete?: ReferenceOptions["onDelete"] };
function synchronous(value: unknown): unknown {
  if (value !== null && (typeof value === "object" || typeof value === "function") && typeof (value as {then?: unknown}).then === "function") {
    Promise.resolve(value).catch(() => {});
    throw new TypeError("Field codecs must be synchronous");
  }
  return value;
}

/** Immutable application validator with an explicit storage representation. */
class Field<T, N extends boolean = false, D extends boolean = false, R extends ReferenceTarget | undefined = undefined, S extends Scalar = Scalar, B = T> implements Validator<Nullable<T, N>> {
  readonly [fieldBrand] = true;
  declare readonly $types: { value: T; nullable: N; default: D; reference: R; base: B };
  readonly scalar: S;
  readonly allowsNull: N;
  readonly hasDefault: D;
  readonly defaultValue: Nullable<T, N> | undefined;
  readonly target: R;
  readonly jsonShape?: JsonShape;
  readonly precision?: number;
  readonly mode?: "number" | "bigint" | "string" | "date";
  readonly customCodec: boolean;
  readonly constraints?: FieldConstraints;
  readonly onDelete?: ReferenceOptions["onDelete"];
  constructor(scalar: S, allowsNull: N, hasDefault: D, defaultValue: Nullable<T, N> | undefined, target: R, jsonShape?: JsonShape, representation?: Representation, options: FieldOptions = {}) {
    this.scalar = scalar; this.allowsNull = allowsNull; this.hasDefault = hasDefault;
    this.defaultValue = defaultValue; this.target = target;
    if (jsonShape) this.jsonShape = Object.freeze(jsonShape);
    if (options.precision !== undefined) this.precision = options.precision;
    if (options.mode !== undefined) this.mode = options.mode;
    this.customCodec = options.customCodec ?? false;
    if (options.constraints) this.constraints = Object.freeze({ ...options.constraints, ...(options.constraints.oneOf ? { oneOf: Object.freeze([...options.constraints.oneOf]) } : {}) });
    if (options.onDelete) this.onDelete = options.onDelete;
    representations.set(this, representation ?? { encode: value => value, decode: value => value });
    Object.freeze(this);
  }
  private copy<NewT, NewN extends boolean, NewD extends boolean, NewR extends ReferenceTarget | undefined>(allowsNull: NewN, hasDefault: NewD, defaultValue: Nullable<NewT, NewN> | undefined, target: NewR, representation = representations.get(this)!, customCodec = this.customCodec, constraints = this.constraints): Field<NewT, NewN, NewD, NewR, S, B> {
    return new Field(this.scalar, allowsNull, hasDefault, defaultValue, target, this.jsonShape, representation, { ...(this.precision === undefined ? {} : { precision: this.precision }), ...(this.mode === undefined ? {} : { mode: this.mode }), ...(this.onDelete === undefined ? {} : { onDelete: this.onDelete }), ...(constraints === undefined ? {} : { constraints }), customCodec });
  }
  optional(): JsonValidator<Nullable<T, N>, true> { return jsonValidator(this).optional(); }
  nullable(): Field<T, true, D, R, S, B> & Pick<this, "customCodec"> { return this.copy<T, true, D, R>(true, this.hasDefault, this.defaultValue, this.target) as never; }
  default(value: Nullable<T, N>): Field<T, N, true, R, S, B> & Pick<this, "customCodec"> { return this.copy<T, N, true, R>(this.allowsNull, true, this.target === undefined ? this.parse(value) : value, this.target) as never; }
  min(value: R extends undefined ? S extends "int64" ? number | bigint : S extends "string" | "float64" ? number : never : never): Field<T, N, D, R, S, B> & Pick<this, "customCodec"> { return this.bound("min", value) as never; }
  max(value: R extends undefined ? S extends "int64" ? number | bigint : S extends "string" | "float64" ? number : never : never): Field<T, N, D, R, S, B> & Pick<this, "customCodec"> { return this.bound("max", value) as never; }
  private bound(kind: "min" | "max", value: number | bigint): Field<T, N, D, R, S, B> {
    if (this.target !== undefined) throw new TypeError("v.id inherits its target primary key constraints; configure the target primary key or a named SQL check");
    if (!["string", "float64", "int64"].includes(this.scalar)) throw new TypeError("Bounds require a string or numeric field");
    if (this.scalar === "int64") {
      if (typeof value === "number" && !Number.isSafeInteger(value)) throw new TypeError("int64 bounds require safe integers or bigint");
      value = this.storage(typeof value === "number" ? BigInt(value) : value) as bigint;
    } else if (typeof value !== "number" || !Number.isFinite(value) || (this.scalar === "string" && (!Number.isSafeInteger(value) || value < 0))) throw new TypeError("Invalid field bound");
    const previous = this.constraints?.[kind];
    // Chaining refines constraints; it never silently weakens an earlier check.
    if (previous !== undefined) value = kind === "min" ? (value > previous ? value : previous) : (value < previous ? value : previous);
    return this.constrained({ ...this.constraints, [kind]: value });
  }
  oneOf<const Values extends readonly T[]>(values: R extends undefined ? S extends "json" ? never : Values : never): Field<Values[number], N, D, R, S, B> & Pick<this, "customCodec"> {
    if (this.target !== undefined) throw new TypeError("v.id inherits its target primary key constraints; configure the target primary key or a named SQL check");
    if (this.scalar === "json" || !Array.isArray(values) || !values.length) throw new TypeError("oneOf requires a nonempty scalar value list");
    const stored = [...new Set(values.map(value => this.encode(value)))];
    if (stored.some(value => value === null || !["string", "number", "bigint", "boolean"].includes(typeof value))) throw new TypeError("oneOf values must be non-null scalars");
    return this.constrained({ ...this.constraints, oneOf: stored as (string | number | bigint | boolean)[] }) as never;
  }
  private constrained(constraints: FieldConstraints): Field<T, N, D, R, S, B> {
    if (constraints.min !== undefined && constraints.max !== undefined && constraints.min > constraints.max) throw new TypeError("Contradictory field bounds");
    const field = this.copy<T, N, D, R>(this.allowsNull, this.hasDefault, this.defaultValue, this.target, representations.get(this)!, this.customCodec, constraints);
    if (constraints.oneOf) for (const value of constraints.oneOf) field.checkConstraints(value);
    if (field.hasDefault) field.parse(field.defaultValue);
    return field;
  }
  private checkConstraints(value: unknown): void {
    if (value === null || !this.constraints) return;
    const measured = this.scalar === "string" ? [...value as string].length : value as number | bigint;
    if (this.constraints.min !== undefined && measured < this.constraints.min) throw new TypeError("Value is below field minimum");
    if (this.constraints.max !== undefined && measured > this.constraints.max) throw new TypeError("Value exceeds field maximum");
    if (this.constraints.oneOf && !this.constraints.oneOf.includes(value as never)) throw new TypeError("Value is not in field oneOf values");
  }
  codec<App>(codec: FieldCodec<App, T>): Field<App, N, D, R, S, S extends "date" | "timestamp" | "localTimestamp" ? string : B> & { readonly customCodec: true } {
    if (this.target !== undefined) throw new TypeError("v.id inherits its target primary key codec; configure the codec on the primary key");
    if (!codec || typeof codec.encode !== "function" || typeof codec.decode !== "function") throw new TypeError("Expected encode and decode codec functions");
    const prior = representations.get(this)!;
    const representation = { encode: (value: unknown) => prior.encode(synchronous(codec.encode(value as App))), decode: (value: unknown) => synchronous(codec.decode(prior.decode(value) as T)) };
    const defaultValue = this.hasDefault && this.defaultValue !== null ? synchronous(codec.decode(this.defaultValue as T)) : this.defaultValue;
    return this.copy<App, N, D, R>(this.allowsNull, this.hasDefault, defaultValue as Nullable<App, N> | undefined, this.target, representation, true) as never;
  }
  /** Validate and encode an application value to the database scalar (int64 uses bigint). */
  encode(value: unknown): unknown {
    if (value === null) { if (this.allowsNull) return null; throw new TypeError("Null requires a nullable field"); }
    const stored = this.storage(representations.get(this)!.encode(value)); this.checkConstraints(stored); return stored;
  }
  /** Predicates and arithmetic operands validate representation, not stored-row constraints. */
  encodeOperand(value: unknown): unknown {
    if (value === null) { if (this.allowsNull) return null; throw new TypeError("Null requires a nullable field"); }
    return this.storage(representations.get(this)!.encode(value));
  }
  /** Validate and decode a database wire value. */
  decode(value: unknown): Nullable<T, N> {
    if (value === null) { if (this.allowsNull) return null as Nullable<T, N>; throw new TypeError("Null requires a nullable field"); }
    if (this.scalar === "int64") {
      if (typeof value !== "string" || !/^(0|-?[1-9][0-9]*)$/.test(value)) throw new TypeError("Invalid stored int64");
      value = BigInt(value);
    }
    const stored = this.storage(value); this.checkConstraints(stored);
    return synchronous(representations.get(this)!.decode(stored)) as Nullable<T, N>;
  }
  /** Storage-level validation is also used by SQL arithmetic operands. */
  storage(value: unknown): unknown {
    if (value === null && this.allowsNull) return null;
    if (this.scalar === "json") { if (value === null) throw new TypeError("Root null requires nullable JSON field"); return validateJson(value, this.jsonShape); }
    if (this.scalar === "date") return dateString(value);
    if (this.scalar === "timestamp" || this.scalar === "localTimestamp") return timestampString(value, this.scalar === "timestamp", this.precision ?? 3);
    const valid = this.scalar === "float64" ? typeof value === "number" && Number.isFinite(value)
      : this.scalar === "string" ? typeof value === "string" && !/[\uD800-\uDFFF]/u.test(value)
      : this.scalar === "boolean" ? typeof value === "boolean"
      : this.scalar === "int64" ? typeof value === "bigint" && value >= -(1n << 63n) && value < 1n << 63n
      : typeof value === "string" && /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/.test(value);
    if (!valid) throw new TypeError(`Expected a ${this.scalar}${this.allowsNull ? " or null" : ""}`);
    return this.scalar === "uuid" ? (value as string).toLowerCase() : value;
  }
  encodeElement(value: unknown): unknown { return encodeJsonElement(this.jsonShape, value); }
  nested(parts: readonly string[]): { scalar: string; jsonShape: JsonShape; parse(value: unknown): unknown; encode(value: unknown): unknown; encodeOperand(value: unknown): unknown; encodeElement(value: unknown): unknown } | undefined {
    let shape = this.jsonShape;
    for (const part of parts) {
      if (shape?.kind === "nullable") shape = shape.inner;
      if (shape?.kind === "any") return { scalar: "json", jsonShape: shape, parse: value => validateJson(value), encode: value => validateJson(value), encodeOperand: value => validateJson(value), encodeElement: value => validateJson(value) };
      if (shape?.kind !== "object" || !Object.hasOwn(shape.fields, part)) return undefined;
      shape = shape.fields[part]!.shape;
    }
    if (!shape) return undefined;
    const validator = runtimeJsonValidators.get(shape);
    const scalar = shape.kind === "nullable" ? shape.inner : shape;
    return { scalar: scalar.kind === "number" ? "float64" : scalar.kind === "string" || scalar.kind === "boolean" ? scalar.kind : "json", jsonShape: shape, parse: value => validator ? validator.parse(value) : validateJson(value, shape), encode: value => validator ? validator.encode(value) : validateJson(value, shape), encodeOperand: value => validator ? validator.encode(value) : validateJson(value, shape), encodeElement: value => encodeJsonElement(shape, value) };
  }
  parse(value: unknown): Nullable<T, N> {
    const stored = this.encode(value);
    if (value === null || this.customCodec) return value as Nullable<T, N>;
    return representations.get(this)!.decode(stored) as Nullable<T, N>;
  }
}

function dateString(value: unknown): string {
  if (typeof value !== "string" || !/^[0-9]{4}-[0-9]{2}-[0-9]{2}$/.test(value) || value.startsWith("0000")) throw new TypeError("Expected YYYY-MM-DD calendar date (years 0001..9999)");
  const time = new Date(value + "T00:00:00.000Z");
  if (!Number.isFinite(time.getTime()) || time.toISOString().slice(0, 10) !== value) throw new TypeError("Invalid calendar date");
  return value;
}
function validDate(value: unknown): Date {
  if (!(value instanceof Date) || !Number.isFinite(value.getTime()) || value.getUTCFullYear() < 1 || value.getUTCFullYear() > 9999) throw new TypeError("Expected a valid Date (years 0001..9999)");
  return value;
}
function timestampString(value: unknown, timezone: boolean, precision: number): string {
  if (typeof value !== "string") throw new TypeError("Expected timestamp string");
  const pattern = timezone ? /^([0-9]{4}-[0-9]{2}-[0-9]{2})T([0-9]{2}):([0-9]{2}):([0-9]{2})(?:\.([0-9]+))?(Z|[+-][0-9]{2}:[0-9]{2})$/ : /^([0-9]{4}-[0-9]{2}-[0-9]{2})T([0-9]{2}):([0-9]{2}):([0-9]{2})(?:\.([0-9]+))?$/;
  const match = pattern.exec(value);
  if (!match) throw new TypeError("Expected ISO timestamp with explicit timezone" + (timezone ? "" : "-free clock components"));
  dateString(match[1]);
  if (Number(match[2]) > 23 || Number(match[3]) > 59 || Number(match[4]) > 59) throw new TypeError("Invalid timestamp clock components");
  const fraction = match[5] ?? "";
  if (fraction.length > 9 || fraction.slice(precision).replaceAll("0", "")) throw new TypeError("Timestamp exceeds configured precision");
  const timezonePart = match[6] ?? "Z";
  if (timezonePart !== "Z" && (Number(timezonePart.slice(1, 3)) > 23 || Number(timezonePart.slice(4)) > 59)) throw new TypeError("Invalid timestamp offset");
  const instant = new Date(`${match[1]}T${match[2]}:${match[3]}:${match[4]}.${fraction.slice(0, 3).padEnd(3, "0")}${timezonePart}`);
  validDate(instant);
  const clock = timezone ? instant.toISOString().slice(0, 19) : `${match[1]}T${match[2]}:${match[3]}:${match[4]}`;
  return clock + (precision ? "." + fraction.slice(0, precision).padEnd(precision, "0") : "") + (timezone ? "Z" : "");
}
type Precision = 0 | 1 | 2 | 3 | 4 | 5 | 6;
type DatePrecision = 0 | 1 | 2 | 3;
type TimestampOptions = { withTimezone?: true; mode?: "date"; precision?: DatePrecision } | { withTimezone?: false; mode: "date"; precision?: DatePrecision } | { withTimezone?: boolean; mode: "string"; precision?: Precision } | { withTimezone: false; mode?: "string"; precision?: Precision };
function timestamp<O extends TimestampOptions = {}>(options: O = {} as O): Field<O extends { mode: "string" } ? string : O extends { withTimezone: false; mode?: "string" } ? string : Date, false, false, undefined, O extends { withTimezone: false } ? "localTimestamp" : "timestamp"> {
  const precision = options.precision ?? 3;
  const mode = options.mode ?? (options.withTimezone === false ? "string" : "date");
  if (!Number.isInteger(precision) || precision < 0 || precision > 6 || !["string", "date"].includes(mode) || (mode === "date" && precision > 3)) throw new TypeError("Timestamp Date mode requires precision 0..3; use string mode for precision 4..6");
  const timezone = options.withTimezone !== false;
  if (options.withTimezone !== undefined && typeof options.withTimezone !== "boolean") throw new TypeError("withTimezone must be boolean");
  return new Field(timezone ? "timestamp" : "localTimestamp", false, false, undefined, undefined, undefined, { encode: value => timestampString(mode === "date" ? validDate(value).toISOString().replace(timezone ? /$/ : /Z$/, "") : value, timezone, precision), decode: value => mode === "date" ? new Date(String(value) + (timezone ? "" : "Z")) : value }, { precision, mode }) as never;
}
function int64<O extends { mode?: "number" | "bigint" } = {}>(options: O = {} as O): Field<O extends { mode: "bigint" } ? bigint : number, false, false, undefined, "int64"> {
  const mode = options.mode ?? "number";
  if (!["number", "bigint"].includes(mode)) throw new TypeError("Invalid int64 representation mode");
  return new Field("int64", false, false, undefined, undefined, undefined, {
    encode: value => { if (mode === "bigint") return value; if (typeof value !== "number" || !Number.isSafeInteger(value)) throw new TypeError("int64 number mode requires a safe integer; use mode: bigint for lossless values"); return BigInt(value); },
    decode: value => { if (mode === "bigint") return value; const number = Number(value); if (!Number.isSafeInteger(number)) throw new TypeError("Stored int64 exceeds safe integer range; use mode: bigint"); return number; },
  }, { mode }) as never;
}
function date<O extends { mode?: "string" | "date" } = {}>(options: O = {} as O): Field<O extends { mode: "date" } ? Date : string, false, false, undefined, "date"> {
  const mode = options.mode ?? "string";
  if (!["string", "date"].includes(mode)) throw new TypeError("Invalid date representation mode");
  return new Field("date", false, false, undefined, undefined, undefined, { encode: value => mode === "date" ? validDate(value).toISOString().slice(0, 10) : value, decode: value => mode === "date" ? new Date(value + "T00:00:00.000Z") : value }, { mode }) as never;
}

type ShapeInput = Field<any, any, any, any, any, any> | JsonValidator<any, any, any>;
type ShapeFields = Readonly<Record<string, ShapeInput>>;
type ShapeValue<S extends ShapeInput> = S extends Field<infer T, infer N, any, any, any, any> ? Nullable<T, N> : S extends JsonValidator<infer T, any, any> ? T : never;
type ObjectValue<F extends ShapeFields> = { -readonly [K in keyof F as F[K] extends JsonValidator<any, true, any> ? never : K]: ShapeValue<F[K]> } & { -readonly [K in keyof F as F[K] extends JsonValidator<any, true, any> ? K : never]?: ShapeValue<F[K]> };
type ShapeBase<S extends ShapeInput> = S extends Field<any, infer N, any, any, any, infer B> ? Nullable<B, N> : S extends JsonValidator<any, any, infer B> ? B : never;
type ObjectBase<F extends ShapeFields> = { -readonly [K in keyof F as F[K] extends JsonValidator<any, true, any> ? never : K]: ShapeBase<F[K]> } & { -readonly [K in keyof F as F[K] extends JsonValidator<any, true, any> ? K : never]?: ShapeBase<F[K]> };
const runtimeJsonValidators = new WeakMap<JsonShape, JsonValidator<any, any, any>>();
class JsonValidator<T, O extends boolean = false, B = T> implements Validator<T> {
  declare readonly $types: { value: T; base: B };
  readonly shape: JsonShape;
  readonly isOptional: O;
  readonly customCodec: boolean;
  readonly encode: (value: unknown, path?: string) => unknown;
  readonly decode: (value: unknown, path?: string) => T;
  constructor(shape: JsonShape, isOptional: O = false as O, encode: (value: unknown, path?: string) => unknown = (value, path) => validateJson(value, shape, path), decode: (value: unknown, path?: string) => T = (value, path) => validateJson(value, shape, path) as T, customCodec = false) {
    this.shape = Object.freeze(shape); runtimeJsonValidators.set(this.shape, this); this.isOptional = isOptional; this.encode = encode; this.decode = decode; this.customCodec = customCodec; Object.freeze(this);
  }
  nullable(): JsonValidator<T | null, O, B | null> { return new JsonValidator({ kind: "nullable", inner: this.shape }, this.isOptional, (value, path) => value === null ? null : this.encode(value, path), (value, path) => value === null ? null : this.decode(value, path), this.customCodec); }
  optional(): JsonValidator<T, true, B> { return new JsonValidator(this.shape, true, this.encode, this.decode, this.customCodec); }
  parse(value: unknown): T { return this.decode(this.encode(value)); }
}
/** Internal adapter bridge; the immutable shape is the runtime validator identity. */
export function jsonValidatorForShape(shape: JsonShape): { readonly customCodec: boolean; parse(value: unknown): unknown } | undefined { return runtimeJsonValidators.get(shape); }
function encodeJsonElement(shape: JsonShape | undefined, value: unknown): unknown {
  if (shape?.kind === "nullable") shape = shape.inner;
  if (shape?.kind !== "array") throw new TypeError("Expected an array JSON shape");
  const validator = runtimeJsonValidators.get(shape.item);
  return validateJson(validator ? validator.encode(value) : value, shape.item);
}
function jsonValidator(input: ShapeInput): JsonValidator<any, any, any> {
  if (input instanceof JsonValidator) return input;
  if (!(input instanceof Field) || !["string", "boolean", "float64"].includes(input.scalar) || input.hasDefault || input.target !== undefined) throw new TypeError("JSON shapes require JSON-compatible validators; use an explicit string/number codec for Date, bigint or other application values");
  if (input.constraints) throw new TypeError("Field constraints require a database column; use a SQL check or function input validation for JSON members");
  const scalar: JsonShape = { kind: input.scalar === "float64" ? "number" : input.scalar as "string" | "boolean" };
  const shape: JsonShape = input.allowsNull ? { kind: "nullable", inner: scalar } : scalar;
  return new JsonValidator(shape, false, value => input.encode(value), value => input.decode(value), input.customCodec);
}
function objectShape<F extends ShapeFields>(fields: F): JsonValidator<ObjectValue<F>, false, ObjectBase<F>> {
  plain(fields, "JSON shape");
  if (Object.keys(fields).some(key => !key)) throw new TypeError("JSON shape keys must be nonempty");
  const children = Object.fromEntries(Object.entries(fields).map(([key, input]) => [key, jsonValidator(input)]));
  const shape: JsonShape = { kind: "object", fields: Object.freeze(Object.fromEntries(Object.entries(children).map(([key, input]) => [key, Object.freeze({ shape: input.shape, optional: input.isOptional })]))) };
  function map(value: unknown, direction: "encode" | "decode", path = "$"): unknown {
    plain(value, "JSON object");
    if (Object.getOwnPropertySymbols(value).length) throw new TypeError("Symbol properties are not JSON");
    const result: Record<string, unknown> = Object.create(null);
    for (const key of Object.getOwnPropertyNames(value)) {
      const property = Object.getOwnPropertyDescriptor(value, key)!;
      if (!property.enumerable || !("value" in property)) throw new TypeError("JSON accessors/nonenumerable properties are unsupported");
      if (!Object.hasOwn(children, key)) throw new TypeError(`${path}.${key}: Unknown property`);
      result[key] = children[key]![direction](property.value, `${path}.${key}`);
    }
    for (const [key, child] of Object.entries(children)) if (!child.isOptional && !Object.hasOwn(result, key)) throw new TypeError(`${path}.${key}: Missing property`);
    return result;
  }
  return new JsonValidator(shape, false, (value, path) => validateJson(map(value, "encode", path), shape, path), (value, path) => map(validateJson(value, shape, path), "decode", path) as ObjectValue<F>);
}
function arrayShape<S extends ShapeInput>(item: S): JsonValidator<ShapeValue<S>[], false, ShapeBase<S>[]> {
  const child = jsonValidator(item);
  const shape: JsonShape = { kind: "array", item: child.shape };
  function encode(value: unknown, path = "$"): unknown {
    if (!Array.isArray(value) || Object.getOwnPropertySymbols(value).length || Object.getOwnPropertyNames(value).length !== value.length + 1) throw new TypeError("Expected JSON array");
    return validateJson(Array.from({ length: value.length }, (_, i) => {
      const property = Object.getOwnPropertyDescriptor(value, String(i));
      if (!property || !("value" in property)) throw new TypeError("Sparse arrays and accessors are not JSON");
      return child.encode(property.value, `${path}[${i}]`);
    }), shape);
  }
  return new JsonValidator(shape, false, encode, (value, path = "$") => (validateJson(value, shape, path) as JsonValue[]).map((item, i) => child.decode(item, `${path}[${i}]`)));
}
function jsonField(): Field<JsonValue, false, false, undefined, "json">;
function jsonField<T>(): Field<T, false, false, undefined, "json">;
function jsonField<S extends ShapeInput>(shape: S): Field<ShapeValue<S>, false, false, undefined, "json", ShapeBase<S>>;
function jsonField<F extends ShapeFields>(fields: F): Field<ObjectValue<F>, false, false, undefined, "json", ObjectBase<F>>;
function jsonField(input?: ShapeInput | ShapeFields): Field<any, false, false, undefined, "json"> {
  const validator = input === undefined ? new JsonValidator<JsonValue>({ kind: "any" }) : input instanceof Field || input instanceof JsonValidator ? jsonValidator(input) : objectShape(input);
  return new Field("json", false, false, undefined, undefined, validator.shape, { encode: validator.encode, decode: validator.decode });
}

function tableKeyField(table: TableShape): AnyField {
  return table.fields[table.options.primaryKey ?? "id"] ?? v.uuid();
}
function id<const Target extends ReferenceTarget>(target: Target, options: ReferenceOptions = {}): Field<Target extends TableShape ? KeyValue<Target> : string | number | bigint, false, false, Target, Target extends TableShape ? TableFields<Target>[KeyName<Target>]["scalar"] : Scalar> & (Target extends TableShape ? Pick<TableFields<Target>[KeyName<Target>], "customCodec"> : {}) {
  if ((typeof target !== "string" || !target) && (typeof target !== "object" || target?.[tableBrand] !== true)) throw new TypeError("v.id requires a table name or table declaration");
  const onDelete = options.onDelete;
  plain(options, "v.id options");
  if (Object.keys(options).some(key => key !== "onDelete") || (onDelete !== undefined && !["restrict", "cascade", "setNull"].includes(onDelete))) throw new TypeError("Invalid reference deletion policy");
  const key = typeof target === "string" ? v.uuid() : tableKeyField(target);
  return new Field(key.scalar, false, false, undefined, target, undefined, representations.get(key), { ...(key.mode === undefined ? {} : { mode: key.mode }), customCodec: key.customCodec, onDelete: onDelete ?? "restrict" }) as never;
}

export const v = {
  id,
  json: jsonField,
  object: objectShape,
  array: arrayShape,
  number: () => new Field<number, false, false, undefined, "float64">("float64", false, false, undefined, undefined),
  string: () =>
    new Field<string, false, false, undefined, "string">(
      "string",
      false,
      false,
      undefined,
      undefined,
    ),
  boolean: () =>
    new Field<boolean, false, false, undefined, "boolean">(
      "boolean",
      false,
      false,
      undefined,
      undefined,
    ),
  int64,
  date,
  timestamp,
  uuid: () =>
    new Field<string, false, false, undefined, "uuid">(
      "uuid",
      false,
      false,
      undefined,
      undefined,
    ),
};

type AnyField = Field<any, boolean, boolean, ReferenceTarget | undefined, Scalar, any>;
type Fields = Readonly<Record<string, AnyField>>;
type TableInputs = Readonly<Record<string, AnyField | JsonValidator<any, false, any>>>;
type ColumnField<V> = V extends JsonValidator<infer T, false, infer B>
  ? Field<Exclude<T, null>, null extends T ? true : false, false, undefined, "json", Exclude<B, null>> : V extends AnyField ? V : never;
type ColumnFields<F extends TableInputs> = { readonly [K in keyof F]: ColumnField<F[K]> };
export type KeyGeneration =
  | "none"
  | "uuidV4"
  | "uuidV7"
  | Readonly<{ handler: string; outputType: Scalar }>;
export interface TableOptions {
  readonly checks?: { invoke(fields: CheckFields<string>): Readonly<Record<string, SqlCheck>> }["invoke"];
  readonly primaryKey?: string;
  readonly keyGeneration?: KeyGeneration;
  readonly insertKeyPolicy?: "allowExplicit" | "generatedOnly";
  readonly unique?: Readonly<
    Record<
      string,
      {
        readonly fields: readonly string[];
        readonly nulls?: "distinct" | "notDistinct";
      }
    >
  >;
}
const tableBrand = Symbol("Hulla schema table");
export interface TableDefinition<
  F extends Fields = Fields,
  O extends TableOptions = TableOptions,
> {
  readonly fields: F;
  readonly options: O;
  readonly [tableBrand]: true;
  readonly indexes: readonly IndexDefinition[];
  readonly $inferSelect: StandaloneRow<TableDefinition<F, O>>;
  readonly $inferInsert: StandaloneInsert<TableDefinition<F, O>>;
  readonly $inferUpdate: StandaloneUpdate<TableDefinition<F, O>>;
  index<const K extends Extract<keyof F, string> | (O extends {readonly primaryKey:string} ? never : "id")>(fields: readonly K[], options?: { readonly name?: string }): TableDefinition<F, O>;
}
export interface IndexDefinition { readonly name?: string; readonly fields: readonly string[] }

function withIndexes<F extends Fields, O extends TableOptions>(fields: F, options: O, indexes: readonly IndexDefinition[]): TableDefinition<F, O> {
  return Object.freeze({ fields, options, indexes: Object.freeze(indexes), [tableBrand]: true as const,
    index(selected: readonly string[], indexOptions: {readonly name?: string} = {}) {
      plain(indexOptions, "table.index");
      if (Object.keys(indexOptions).some(key => key !== "name") || (indexOptions.name !== undefined && (typeof indexOptions.name !== "string" || !indexOptions.name))) throw new TypeError("Invalid index options");
      if (!Array.isArray(selected) || !selected.length || new Set(selected).size !== selected.length) throw new TypeError("Index requires distinct fields");
      for (const name of selected) if (!Object.hasOwn(fields, name) && !(name === "id" && options.primaryKey === undefined)) throw new TypeError(`Unknown index field ${name}`);
      const declaration = Object.freeze({ ...indexOptions, fields: Object.freeze([...selected]) });
      return withIndexes(fields, options, [...indexes, declaration]);
    },
  }) as TableDefinition<F, O>;
}

export function defineTable<
  const F extends TableInputs,
  const O extends Omit<TableOptions, "checks"> = {},
>(
  fields: F,
  options: O & {
    readonly checks?: (fields: CheckFields<Extract<keyof F, string> | "id">) => Readonly<Record<string, SqlCheck>>;
    readonly primaryKey?: keyof F | "id";
    readonly unique?: Readonly<
      Record<
        string,
        {
          readonly fields: readonly (keyof F | "id")[];
          readonly nulls?: "distinct" | "notDistinct";
        }
      >
    >;
  } = {} as O,
): TableDefinition<ColumnFields<F>, O> {
  plain(fields, "table.fields");
  plain(options, "table.options");
  if (options.unique !== undefined) {
    plain(options.unique, "table.unique");
    for (const [name, constraint] of Object.entries(options.unique)) {
      plain(constraint, `table.unique.${name}`);
      if (!Array.isArray(constraint.fields))
        fail(`table.unique.${name}`, "Expected a field list");
      if (
        Object.keys(constraint).some(
          (key) => !["fields", "nulls"].includes(key),
        )
      )
        fail(`table.unique.${name}`, "Unknown unique option");
    }
  }
  const unique =
    options.unique &&
    Object.freeze(
      Object.fromEntries(
        Object.entries(options.unique).map(([name, value]) => [
          name,
          Object.freeze({ ...value, fields: Object.freeze([...value.fields]) }),
        ]),
      ),
    );
  const keyGeneration =
    typeof options.keyGeneration === "object" && options.keyGeneration !== null
      ? Object.freeze({ ...options.keyGeneration })
      : options.keyGeneration;
  const columns = Object.fromEntries(Object.entries(fields).map(([name, field]) => {
    if (!(field instanceof JsonValidator)) return [name, field];
    if (field.isOptional) throw new TypeError("Table columns cannot be optional; use nullable JSON columns");
    const column = jsonField(field);
    return [name, field.shape.kind === "nullable" ? column.nullable() : column];
  })) as ColumnFields<F>;
  return withIndexes(Object.freeze(columns), Object.freeze({
      ...options,
      ...(keyGeneration === undefined ? {} : { keyGeneration }),
      ...(unique ? { unique } : {}),
    }) as O, []);
}

type Tables = Readonly<Record<string, TableShape>>;
const compiledSchemas = new WeakMap<
  SchemaDefinition,
  readonly CompiledTable[]
>();
const compiledDefinitions = new WeakMap<TableShape, CompiledTable>();
export function compiledTableForDefinition(table: TableShape): CompiledTable | undefined { return compiledDefinitions.get(table); }
export interface SchemaDefinition<T extends Tables = Tables> {
  readonly tables: T;
  readonly defaultUuidGeneration: "v4" | "v7";
}

/** Validate authoring metadata immediately; storage enforcement belongs to the adapter. */
export function defineSchema<const T extends Tables>(
  tables: T,
  options: { readonly defaultUuidGeneration?: "v4" | "v7" } = {},
): SchemaDefinition<T> {
  plain(tables, "schema.tables");
  plain(options, "schema.options");
  if (Object.keys(options).some((key) => key !== "defaultUuidGeneration"))
    fail("schema.options", "Unknown schema option");
  const schema = Object.freeze({
    tables: Object.freeze({ ...tables }),
    defaultUuidGeneration: options.defaultUuidGeneration ?? "v7",
  });
  compiledSchemas.set(schema, compileSchema(schema));
  return schema;
}

type KeyName<D extends TableShape> = D["options"] extends {
  readonly primaryKey: infer K extends string;
}
  ? K
  : "id";
type TableFields<D extends TableShape> =
  KeyName<D> extends keyof D["fields"]
    ? D["fields"]
    : D["fields"] & { readonly id: ReturnType<typeof v.uuid> };
declare const idBrand: unique symbol;
type KeyValue<D extends TableShape> =
  TableFields<D>[KeyName<D>] extends AnyField
    ? TableFields<D>[KeyName<D>]["$types"]["value"]
    : never;
type TableSignature<D extends TableShape> = { key: KeyName<D>; fields: { [K in keyof D["fields"]]: Pick<D["fields"][K]["$types"], "value" | "nullable" | "default"> & { scalar: D["fields"][K]["scalar"] } } };
type ReferenceName<S extends SchemaDefinition, R> = string extends keyof S["tables"] ? never : 0 extends 1 & R ? never : R extends keyof S["tables"] ? R : R extends TableShape ? { [K in keyof S["tables"]]: TableSignature<S["tables"][K]> extends TableSignature<R> ? TableSignature<R> extends TableSignature<S["tables"][K]> ? K : never : never }[keyof S["tables"]] : never;
/** Code generation records object-reference identities that TypeScript cannot infer nominally. */
export type ResolveReferences<S extends SchemaDefinition, R extends Readonly<Record<string, Readonly<Record<string, string>>>>> = S & { readonly $references: R };
type FieldReferenceName<S extends SchemaDefinition, T extends keyof S["tables"], K extends keyof TableFields<S["tables"][T]>> = S extends {readonly $references: infer Refs} ? T extends keyof Refs ? K extends keyof Refs[T] ? Extract<Refs[T][K], keyof S["tables"]> : ReferenceName<S, TableFields<S["tables"][T]>[K]["$types"]["reference"]> : ReferenceName<S, TableFields<S["tables"][T]>[K]["$types"]["reference"]> : ReferenceName<S, TableFields<S["tables"][T]>[K]["$types"]["reference"]>;
type SchemaKeyValue<S extends SchemaDefinition, T extends keyof S["tables"], Seen = never> = T extends Seen ? never : FieldReferenceName<S, T, Extract<KeyName<S["tables"][T]>, keyof TableFields<S["tables"][T]>>> extends infer R extends keyof S["tables"] ? [R] extends [never] ? KeyValue<S["tables"][T]> : SchemaKeyValue<S, R, Seen | T> : never;
/** Accept validated scalar inputs while preserving the table provenance of known IDs. */
export type Id<
  S extends SchemaDefinition,
  T extends keyof S["tables"],
> = SchemaKeyValue<S, T> & { readonly [idBrand]?: T };
type FieldValue<S extends SchemaDefinition, F extends AnyField, Reference = ReferenceName<S, F["$types"]["reference"]>> = Nullable<
  [Reference] extends [infer R extends keyof S["tables"]]
    ? [R] extends [never] ? F["$types"]["value"] : Id<S, R>
    : F["$types"]["value"],
  F["$types"]["nullable"]
>;
export type Row<S extends SchemaDefinition, T extends keyof S["tables"]> = {
  -readonly [K in keyof TableFields<S["tables"][T]>]: K extends KeyName<
    S["tables"][T]
  >
    ? Id<S, T>
    : FieldValue<S, TableFields<S["tables"][T]>[K], FieldReferenceName<S, T, K>>;
};
/** Database operation capabilities before a custom application codec is applied. */
type StoredField<S extends SchemaDefinition, T extends keyof S["tables"], K extends keyof TableFields<S["tables"][T]>, Seen = never, Depth extends readonly unknown[] = []> = Depth["length"] extends 8 ? TableFields<S["tables"][T]>[K] : T extends Seen ? TableFields<S["tables"][T]>[K] : FieldReferenceName<S, T, K> extends infer R extends keyof S["tables"] ? [R] extends [never] ? TableFields<S["tables"][T]>[K] : StoredField<S, R, Extract<KeyName<S["tables"][R]>, keyof TableFields<S["tables"][R]>>, Seen | T, [...Depth, unknown]> : never;
export type StorageRow<S extends SchemaDefinition, T extends keyof S["tables"]> = {
  -readonly [K in keyof TableFields<S["tables"][T]>]: Nullable<StoredField<S, T, K>["$types"]["base"], TableFields<S["tables"][T]>[K]["$types"]["nullable"]>;
};
/** Identity of SQL values used by atomic write references, independent of application modes. */
export type StorageIdentity<S extends SchemaDefinition, T extends keyof S["tables"]> = {
  -readonly [K in keyof TableFields<S["tables"][T]>]: Nullable<{ readonly scalar: StoredField<S, T, K>["scalar"] }, TableFields<S["tables"][T]>[K]["$types"]["nullable"]>;
};
type Generated<D extends TableShape> = D["options"] extends {
  readonly keyGeneration: infer G;
}
  ? G extends "none"
    ? false
    : true
  : TableFields<D>[KeyName<D>]["$types"]["reference"] extends undefined ? KeyValue<D> extends string
    ? TableFields<D>[KeyName<D>]["scalar"] extends "uuid"
      ? true
      : false
    : false : false;
type OptionalInsert<S extends SchemaDefinition, T extends keyof S["tables"]> = {
  [K in keyof TableFields<S["tables"][T]>]: K extends KeyName<S["tables"][T]>
    ? Generated<S["tables"][T]> extends true
      ? K
      : never
    : TableFields<S["tables"][T]>[K]["$types"]["default"] extends true
      ? K
      : never;
}[keyof TableFields<S["tables"][T]>];
type ForbiddenInsert<
  S extends SchemaDefinition,
  T extends keyof S["tables"],
> = S["tables"][T]["options"] extends {
  readonly insertKeyPolicy: "generatedOnly";
}
  ? KeyName<S["tables"][T]>
  : never;
export type Insert<
  S extends SchemaDefinition,
  T extends keyof S["tables"],
> = Omit<InsertRow<S, T>, OptionalInsert<S, T> | ForbiddenInsert<S, T>> &
  { [K in Extract<OptionalInsert<S, T>, keyof InsertRow<S, T>>]?: InsertRow<S, T>[K] | undefined } & {
    [K in ForbiddenInsert<S, T>]?: never;
  };
type InsertRow<S extends SchemaDefinition, T extends keyof S["tables"]> = Omit<Row<S, T>, KeyName<S["tables"][T]>> & { [K in KeyName<S["tables"][T]>]: SchemaKeyValue<S, T> };
export type Patch<
  S extends SchemaDefinition,
  T extends keyof S["tables"],
> = { [K in Exclude<keyof Row<S, T>, KeyName<S["tables"][T]>>]?: Row<S, T>[K] | undefined } & {
  [K in KeyName<S["tables"][T]>]?: never;
};
type StandaloneRow<D extends TableShape> = { -readonly [K in keyof TableFields<D>]: Nullable<TableFields<D>[K]["$types"]["value"], TableFields<D>[K]["$types"]["nullable"]> };
type StandaloneOptional<D extends TableShape> = { [K in keyof TableFields<D>]: K extends KeyName<D> ? Generated<D> extends true ? K : never : TableFields<D>[K]["$types"]["default"] extends true ? K : never }[keyof TableFields<D>];
type StandaloneForbidden<D extends TableShape> = D["options"] extends {readonly insertKeyPolicy: "generatedOnly"} ? KeyName<D> : never;
type StandaloneInsert<D extends TableShape> = Omit<StandaloneRow<D>, StandaloneOptional<D> | StandaloneForbidden<D>> & { [K in Extract<StandaloneOptional<D>, keyof StandaloneRow<D>>]?: StandaloneRow<D>[K] | undefined } & { [K in StandaloneForbidden<D>]?: never };
type StandaloneUpdate<D extends TableShape> = { [K in Exclude<keyof StandaloneRow<D>, KeyName<D>>]?: StandaloneRow<D>[K] | undefined } & { [K in KeyName<D>]?: never };

export interface CompiledTable {
  readonly indexes: readonly { readonly name: string; readonly fields: readonly string[] }[];
  readonly checks: readonly CompiledCheck[];
  readonly name: string;
  readonly fields: Readonly<Record<string, AnyField>>;
  readonly primaryKey: string;
  readonly keyGeneration: KeyGeneration;
  readonly insertKeyPolicy: "allowExplicit" | "generatedOnly";
  readonly unique: Readonly<
    Record<
      string,
      {
        readonly fields: readonly string[];
        readonly nulls: "distinct" | "notDistinct";
      }
    >
  >;
}

function fail(path: string, message: string): never {
  throw new TypeError(`${path}: ${message}`);
}
function plain(
  value: unknown,
  path: string,
): asserts value is Record<string, unknown> {
  if (
    typeof value !== "object" ||
    value === null ||
    Array.isArray(value) ||
    ![Object.prototype, null].includes(Object.getPrototypeOf(value))
  )
    fail(path, "Expected a plain object");
}

/** Normalize metadata with the same key/default/reference rules as hulladb_schema. */
export function compileSchema(
  schema: SchemaDefinition,
): readonly CompiledTable[] {
  const cached = compiledSchemas.get(schema);
  if (cached) return cached;
  plain(schema, "schema");
  plain(schema.tables, "schema.tables");
  if (!["v4", "v7"].includes(schema.defaultUuidGeneration))
    fail("schema", "Invalid UUID generation");
  const tables: CompiledTable[] = Object.entries(schema.tables).map(
    ([name, table]) => {
      if (!name || table?.[tableBrand] !== true)
        fail(name || "schema", "Expected a named table declaration");
      const options = table.options;
      for (const key of Object.keys(options))
        if (
          ![
            "primaryKey",
            "keyGeneration",
            "insertKeyPolicy",
            "unique",
            "checks",
          ].includes(key)
        )
          fail(name, `Unknown table option ${key}`);
      const fields: Record<string, AnyField> = Object.assign(
        Object.create(null),
        table.fields,
      );
      for (const [key, field] of Object.entries(fields)) {
        if (!key || field?.[fieldBrand] !== true)
          fail(`${name}.${key}`, "Expected a named scalar field");
        if (!["string", "boolean", "int64", "uuid", "float64", "json", "date", "timestamp", "localTimestamp"].includes(field.scalar))
          fail(`${name}.${key}`, "Unknown scalar");
        if (
          typeof field.allowsNull !== "boolean" ||
          typeof field.hasDefault !== "boolean"
        )
          fail(`${name}.${key}`, "Invalid field descriptor");
        if (field.hasDefault && field.target === undefined) field.parse(field.defaultValue);
      }
      const primaryKey = options.primaryKey ?? "id";
      if (typeof primaryKey !== "string" || !primaryKey)
        fail(name, "Expected a single primary key name");
      if (!Object.hasOwn(fields, primaryKey)) {
        if (primaryKey !== "id")
          fail(`${name}.${primaryKey}`, "Missing primary key field");
        fields.id = v.uuid();
      }
      const key = fields[primaryKey]!;
      if (key.allowsNull || key.hasDefault || !["string", "int64", "uuid"].includes(key.scalar))
        fail(
          `${name}.${primaryKey}`,
          "Primary keys must be non-null string, int64 or UUID without a literal default",
        );
      const keyGeneration =
        options.keyGeneration ??
        (key.target !== undefined ? "none" : key.scalar === "uuid"
          ? schema.defaultUuidGeneration === "v4"
            ? "uuidV4"
            : "uuidV7"
          : "none");
      if (typeof keyGeneration === "object") {
        plain(keyGeneration, name);
        if (
          typeof keyGeneration.handler !== "string" ||
          !keyGeneration.handler ||
          !["string", "boolean", "int64", "uuid"].includes(
            keyGeneration.outputType,
          ) ||
          Object.keys(keyGeneration).some(
            (key) => !["handler", "outputType"].includes(key),
          )
        )
          fail(name, "Invalid custom generator");
      } else if (!["none", "uuidV4", "uuidV7"].includes(keyGeneration))
        fail(name, "Invalid key generation");
      const outputType =
        typeof keyGeneration === "object"
          ? keyGeneration.outputType
          : keyGeneration === "none"
            ? undefined
            : "uuid";
      if (outputType !== undefined && key.target === undefined && outputType !== key.scalar)
        fail(name, "Key generator output type does not match primary key");
      const insertKeyPolicy = options.insertKeyPolicy ?? "allowExplicit";
      if (!["allowExplicit", "generatedOnly"].includes(insertKeyPolicy))
        fail(name, "Invalid insert key policy");
      if (insertKeyPolicy === "generatedOnly" && keyGeneration === "none")
        fail(name, "Generated-only keys need a generator");
      const unique: Record<
        string,
        { fields: readonly string[]; nulls: "distinct" | "notDistinct" }
      > = Object.create(null);
      const signatures = new Set<string>();
      for (const [constraint, declaration] of Object.entries(
        options.unique ?? {},
      )) {
        const selected = declaration.fields;
        if (
          !constraint ||
          !Array.isArray(selected) ||
          !selected.length ||
          new Set(selected).size !== selected.length
        )
          fail(
            `${name}.${constraint}`,
            "Unique constraint needs distinct fields",
          );
        for (const field of selected)
          if (!Object.hasOwn(fields, field))
            fail(`${name}.${constraint}`, `Unknown unique field ${field}`);
        const signature = JSON.stringify([...selected].sort());
        if (signatures.has(signature))
          fail(`${name}.${constraint}`, "Duplicate unique field set");
        signatures.add(signature);
        if (
          selected.some((field) => fields[field]!.allowsNull) &&
          declaration.nulls === undefined
        )
          fail(
            `${name}.${constraint}`,
            "Nullable unique fields require an explicit null policy",
          );
        const nulls = declaration.nulls ?? "notDistinct";
        if (!["distinct", "notDistinct"].includes(nulls))
          fail(`${name}.${constraint}`, "Invalid unique null policy");
        unique[constraint] = Object.freeze({
          fields: Object.freeze([...selected]),
          nulls,
        });
      }
      const indexNames = new Set<string>();
      const indexFields = new Set<string>();
      const indexes = table.indexes.map(index => {
        for (const field of index.fields) if (!Object.hasOwn(fields, field)) fail(`${name}.index`, `Unknown index field ${field}`);
        const indexName = index.name ?? defaultIndexName(name, index.fields);
        const signature = JSON.stringify(index.fields);
        if (indexNames.has(indexName) || indexFields.has(signature)) fail(`${name}.index`, "Duplicate index name or field set");
        if (utf8Length(indexName) > 63) fail(`${name}.index`, "Index names must fit PostgreSQL's 63-byte identifier limit");
        indexNames.add(indexName); indexFields.add(signature);
        return Object.freeze({ name: indexName, fields: index.fields });
      });
      return Object.freeze({
        name,
        fields: Object.freeze(fields),
        primaryKey,
        keyGeneration:
          typeof keyGeneration === "object"
            ? Object.freeze({ ...keyGeneration })
            : keyGeneration,
        insertKeyPolicy,
        unique: Object.freeze(unique),
        indexes: Object.freeze(indexes),
        checks: compileChecks(options.checks, Object.keys(fields), primaryKey),
      });
    },
  );
  const byName = new Map(tables.map((table) => [table.name, table]));
  const definitions = new Map<TableShape, string[]>();
  for (const [name, declaration] of Object.entries(schema.tables)) definitions.set(declaration, [...definitions.get(declaration) ?? [], name]);
  const resolvedFields = new Map<string, AnyField>();
  const resolvingFields = new Set<string>();
  const resolveField = (table: CompiledTable, name: string): AnyField => {
      const field = table.fields[name]!;
      const path = JSON.stringify([table.name, name]);
      if (field.target === undefined) return field;
      if (resolvedFields.has(path)) return resolvedFields.get(path)!;
      if (resolvingFields.has(path)) fail(`${table.name}.${name}`, "Cyclic primary-key references cannot resolve a storage scalar");
      resolvingFields.add(path);
      const names = typeof field.target === "string" ? [field.target] : definitions.get(field.target);
      if (!names || names.length !== 1) fail(`${table.name}.${name}`, "Reference table must be registered exactly once in defineSchema");
      const targetName = names[0]!;
      const target = byName.get(targetName);
      if (!target)
        fail(
          `${table.name}.${name}`,
          `Missing reference target ${targetName}`,
        );
      const key = resolveField(target, target.primaryKey);
      if (field.onDelete === "setNull" && !field.allowsNull) fail(`${table.name}.${name}`, "setNull references require a nullable field");
      const resolvedField = new Field(key.scalar, field.allowsNull, field.hasDefault, field.defaultValue, targetName, undefined, representations.get(key), { ...(key.mode === undefined ? {} : {mode:key.mode}), customCodec:key.customCodec, onDelete: field.onDelete ?? "restrict", ...(field.constraints === undefined ? {} : {constraints:field.constraints}) });
      if (resolvedField.hasDefault) resolvedField.parse(resolvedField.defaultValue);
      resolvingFields.delete(path);
      resolvedFields.set(path, resolvedField);
      return resolvedField;
  };
  const resolved = tables.map(table => {
    const fields = Object.fromEntries(Object.keys(table.fields).map(name => [name, resolveField(table, name)]));
    const key = fields[table.primaryKey]!;
    const outputType = typeof table.keyGeneration === "object" ? table.keyGeneration.outputType : table.keyGeneration === "none" ? undefined : "uuid";
    if (outputType !== undefined && outputType !== key.scalar) fail(`${table.name}.${table.primaryKey}`, "Key generator output type does not match primary key");
    const result = Object.freeze({ ...table, fields: Object.freeze(fields) });
    return result;
  });
  const allIndexNames = new Set<string>();
  for (const table of resolved) for (const index of table.indexes) {
    if (allIndexNames.has(index.name)) fail(`${table.name}.index`, `Index name ${index.name} is already used by another table`);
    allIndexNames.add(index.name);
  }
  for (const table of resolved) {
    const prior = compiledDefinitions.get(schema.tables[table.name]!);
    if (prior && !sameTableBinding(prior, table)) fail(table.name, "Table declaration was already bound to an incompatible schema; create a separate declaration for each binding");
  }
  for (const table of resolved) compiledDefinitions.set(schema.tables[table.name]!, table);
  return Object.freeze(resolved);
}

function sameValue(left: unknown, right: unknown): boolean {
  if (Object.is(left, right)) return true;
  if (typeof left !== "object" || left === null || typeof right !== "object" || right === null || Array.isArray(left) !== Array.isArray(right)) return false;
  const keys = Object.keys(left);
  return keys.length === Object.keys(right).length && keys.every(key => Object.hasOwn(right, key) && sameValue((left as Record<string, unknown>)[key], (right as Record<string, unknown>)[key]));
}
function sameTableBinding(left: CompiledTable, right: CompiledTable): boolean {
  return left.primaryKey === right.primaryKey && sameValue(left.keyGeneration, right.keyGeneration) && left.insertKeyPolicy === right.insertKeyPolicy && Object.keys(left.fields).length === Object.keys(right.fields).length && Object.entries(left.fields).every(([name, field]) => {
    const other = right.fields[name];
    return other !== undefined && field.scalar === other.scalar && field.mode === other.mode && field.precision === other.precision && field.allowsNull === other.allowsNull && field.hasDefault === other.hasDefault && field.target === other.target && field.onDelete === other.onDelete && sameValue(field.constraints, other.constraints) && field.customCodec === other.customCodec && (!field.customCodec || representations.get(field) === representations.get(other)) && (!field.hasDefault || sameValue(field.encode(field.defaultValue), other.encode(other.defaultValue)));
  });
}

function defaultIndexName(table: string, fields: readonly string[]): string {
  const full = `${table}_${fields.join("_")}_idx`;
  if (utf8Length(full) <= 63) return full;
  let hash = 2166136261;
  for (const character of JSON.stringify([table, fields])) hash = Math.imul(hash ^ character.codePointAt(0)!, 16777619) >>> 0;
  return `${full.replace(/[^a-zA-Z0-9_]/g, "_").slice(0, 40)}_${hash.toString(16).padStart(8, "0")}_idx`;
}

// Schema evaluation also runs in the restricted JS realm, without Web encoders.
function utf8Length(value: string): number {
  let bytes = 0;
  for (const character of value) {
    const point = character.codePointAt(0)!;
    bytes += point <= 0x7f ? 1 : point <= 0x7ff ? 2 : point <= 0xffff ? 3 : 4;
  }
  return bytes;
}

function parseRecord(
  schema: SchemaDefinition,
  name: string,
  input: unknown,
  kind: "row" | "insert" | "patch",
  fillDefaults = true,
): Record<string, unknown> {
  const table = compileSchema(schema).find((table) => table.name === name);
  if (!table) fail(name, "Unknown table");
  plain(input, name);
  const result: Record<string, unknown> = Object.create(null);
  for (const key of Object.keys(input)) {
    if (!Object.hasOwn(table.fields, key))
      fail(`${name}.${key}`, "Unknown field");
    if (
      key === table.primaryKey &&
      (kind === "patch" ||
        (kind === "insert" && table.insertKeyPolicy === "generatedOnly"))
    )
      fail(`${name}.${key}`, "Primary key assignment forbidden");
  }
  for (const [key, field] of Object.entries(table.fields)) {
    if (Object.hasOwn(input, key)) {
      if (input[key] === undefined && (kind === "patch" || (kind === "insert" && (field.hasDefault || (key === table.primaryKey && table.keyGeneration !== "none"))))) continue;
      try {
        result[key] = field.parse(input[key]);
      } catch (error) {
        fail(`${name}.${key}`, (error as Error).message);
      }
    } else if (kind === "patch") continue;
    else if (kind === "insert" && field.hasDefault) {
      if (fillDefaults) result[key] = field.parse(field.defaultValue);
    }
    else if (
      kind === "insert" &&
      key === table.primaryKey &&
      table.keyGeneration !== "none"
    )
      continue;
    else fail(`${name}.${key}`, "Missing field");
  }
  return result;
}

export function parseRow<
  S extends SchemaDefinition,
  T extends Extract<keyof S["tables"], string>,
>(schema: S, table: T, input: unknown): Row<S, T> {
  return parseRecord(schema, table, input, "row") as Row<S, T>;
}
/** Fill literal defaults; an omitted generated key remains pending runtime execution. */
export function prepareInsert<
  S extends SchemaDefinition,
  T extends Extract<keyof S["tables"], string>,
>(schema: S, table: T, input: unknown, fillDefaults = true): Insert<S, T> {
  return parseRecord(schema, table, input, "insert", fillDefaults) as Insert<S, T>;
}
export function parsePatch<
  S extends SchemaDefinition,
  T extends Extract<keyof S["tables"], string>,
>(schema: S, table: T, input: unknown): Patch<S, T> {
  return parseRecord(schema, table, input, "patch") as Patch<S, T>;
}
