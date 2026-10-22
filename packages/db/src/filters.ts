import { validateJson, type JsonShape } from "./json-value.ts";
import { fieldMetadata, fieldPath, type FieldReference } from "./fields.ts";

const predicateTables = new WeakMap<object, string>();

export type Predicate =
  | { kind: "constant"; value: boolean }
  | { kind: "and" | "or"; predicates: Predicate[] }
  | { kind: "not"; predicate: Predicate }
  | {
      kind: "compare";
      field: string | readonly string[];
      operator: string;
      operand: { kind: "value"; value: unknown } | { kind: "field"; field: string | readonly string[]; transform?: "arrayLength" };
      transform?: "arrayLength";
      caseSensitive?: boolean;
    };

type Key<R> = Extract<keyof R, string>;
export type Paths<R, Depth extends unknown[] = []> = Depth["length"] extends 8 ? never : { [K in Key<R>]: K | (NonNullable<R[K]> extends readonly unknown[] | Date ? never : NonNullable<R[K]> extends object ? `${K}.${Paths<NonNullable<R[K]>, [...Depth, unknown]>}` : never) }[Key<R>];
export type FieldValue<R, F> = F extends FieldReference<infer V> ? V : F extends keyof R ? R[F] : F extends `${infer K}.${infer Rest}` ? K extends keyof R ? FieldValue<NonNullable<R[K]>, Rest> : never : never;
export type Field<R, T extends string, B = R> = Paths<B> | { [K in Paths<B>]: FieldReference<FieldValue<R, K>, T, FieldValue<B, K>> }[Paths<B>];
type Value<R, F> = FieldValue<R, F>;
const numericExpression = Symbol("arrayLength");
export interface NumericExpression<T extends string> extends FieldReference<number | null, T, number | null> { readonly [numericExpression]: true }
type Operand<R, T extends string, F> = Exclude<Value<R, F>, undefined> | null | FieldReference<Value<R, F> | null, T> | (NonNullable<Value<R,F>> extends number ? NumericExpression<T> : never);
type StoredValue<B, F> = F extends FieldReference<unknown, string, infer V> ? V : FieldValue<B, F>;
type Orderable<B, F> = NonNullable<StoredValue<B, F>> extends string | number | bigint | Date ? unknown : never;
type Textual<B, F> = NonNullable<StoredValue<B, F>> extends string ? unknown : never;
type Compare<R, T extends string, B = R> = <F extends Field<R, T, B> | NumericExpression<T>>(
  field: F,
  value: Operand<R, T, NoInfer<F>>,
) => Predicate;
type Ordered<R, T extends string, B = R> = <F extends Field<R, T, B> | NumericExpression<T>>(
  field: F & Orderable<B, F>,
  value: Exclude<Operand<R, T, NoInfer<F>>, boolean | null>,
) => Predicate;
type Text<R, T extends string, B = R> = <F extends Field<R, T, B>>(
  field: F & Textual<B, F>,
  value: string,
  options?: { caseSensitive?: boolean },
) => Predicate;
type Pattern<R, T extends string, B = R> = <F extends Field<R, T, B>>(
  field: F & Textual<B, F>,
  value: string,
) => Predicate;

export interface Expressions<R, T extends string, B = R> {
  eq: Compare<R, T, B>;
  ne: Compare<R, T, B>;
  gt: Ordered<R, T, B>;
  gte: Ordered<R, T, B>;
  lt: Ordered<R, T, B>;
  lte: Ordered<R, T, B>;
  inArray: <F extends Field<R, T, B>>(
    field: F,
    values: readonly (Value<R, NoInfer<F>> | null)[],
  ) => Predicate;
  notInArray: Expressions<R, T, B>["inArray"];
  between: <F extends Field<R, T, B>>(
    field: F & Orderable<B, F>,
    values: readonly [
      Exclude<Value<R, NoInfer<F>>, boolean | null>,
      Exclude<Value<R, NoInfer<F>>, boolean | null>,
    ],
  ) => Predicate;
  isNull: (field: Field<R, T, B>) => Predicate;
  isNotNull: (field: Field<R, T, B>) => Predicate;
  contains: <F extends Field<R, T, B>>(field: F & (NonNullable<StoredValue<B, F>> extends string | readonly unknown[] ? unknown : never), value: NonNullable<Value<R, NoInfer<F>>> extends readonly (infer E)[] ? E : string, options?: NonNullable<StoredValue<B, F>> extends string ? { caseSensitive?: boolean } : never) => Predicate;
  containsAny: ArrayMembers<R, T, B>;
  containsAll: ArrayMembers<R, T, B>;
  containedBy: ArrayMembers<R, T, B>;
  arrayLength: <F extends Field<R, T, B>>(field: F & (NonNullable<Value<R,F>> extends readonly unknown[] ? unknown : never)) => NumericExpression<T>;
  exists: (field: Field<R, T, B>, present?: boolean) => Predicate;
  startsWith: Text<R, T, B>;
  endsWith: Text<R, T, B>;
  like: Pattern<R, T, B>;
  ilike: Pattern<R, T, B>;
  and: (...predicates: Predicate[]) => Predicate;
  or: (...predicates: Predicate[]) => Predicate;
  not: (predicate: Predicate) => Predicate;
}

type ArrayMembers<R, T extends string, B = R> = <F extends Field<R,T,B>>(field: F & (NonNullable<Value<R,F>> extends readonly unknown[] ? unknown : never), values: NonNullable<Value<R,NoInfer<F>>> extends readonly (infer E)[] ? readonly E[] : never) => Predicate;

type TextOptions = { caseSensitive?: boolean };
type FieldDescriptor = { scalar: string; jsonShape?: JsonShape | undefined; parse(value: unknown): unknown; encode?(value: unknown): unknown; encodeOperand?(value: unknown): unknown; encodeElement?(value: unknown): unknown; nested?(parts: readonly string[]): FieldDescriptor | undefined; precision?: number };
type Comparison = Extract<Predicate, { kind: "compare" }>;

const comparisonOperators = ["=", "!=", ">", ">=", "<", "<="];
const rangeOperators = ["between", "not between"];
const listOperators = ["in", "not in", ...rangeOperators];
const literalText = /^(not )?(contains|startsWith|endsWith)$/;
const textOperator = /^(not )?(contains|startsWith|endsWith|like|ilike)$/;
const operators = [
  ...comparisonOperators, ...listOperators, "is", "is not", "exists", "not exists", "containsAny", "not containsAny", "containsAll", "not containsAll", "containedBy", "not containedBy",
  "contains", "not contains", "startsWith", "not startsWith",
  "endsWith", "not endsWith", "like", "not like", "ilike", "not ilike",
];

function validateOptions(options: TextOptions | undefined): void {
  if (options === undefined) return;
  if (
    !options || typeof options !== "object" || Array.isArray(options) ||
    Object.keys(options).some(key => key !== "caseSensitive") ||
    (options.caseSensitive !== undefined && typeof options.caseSensitive !== "boolean")
  ) {
    throw new TypeError("Invalid text comparison options");
  }
}

export function predicateParser(table: string, fields: Record<string, FieldDescriptor>) {
  function mark<P extends Predicate>(predicate: P): P {
    if (predicate.kind === "and" || predicate.kind === "or") {
      Object.freeze(predicate.predicates);
    } else if (predicate.kind === "compare") {
      if (predicate.operand.kind === "value" && Array.isArray(predicate.operand.value)) {
        Object.freeze(predicate.operand.value);
      }
      Object.freeze(predicate.operand);
    }
    predicateTables.set(predicate, table);
    return Object.freeze(predicate);
  }

  function compare(
    input: unknown,
    inputOperator: unknown,
    value: unknown,
    options?: TextOptions,
  ): Predicate {
    validateOptions(options);
    const transformed = typeof input === "object" && input !== null && numericExpression in input;
    if (transformed && (input as { table?: string }).table !== table) throw new TypeError("Expected numeric expression from the selected table");
    const name = transformed ? (input as unknown as { name: string | readonly string[] }).name : fieldPath(input, table);
    function resolve(path: string | readonly string[]): FieldDescriptor | undefined {
      const [root, ...parts] = typeof path === "string" ? Object.hasOwn(fields, path) ? [path] : path.split(".") : path;
      let descriptor = fields[root!];
      if (!descriptor) return undefined;
      if (parts.length && descriptor.nested) return descriptor.nested(parts);
      let shape = descriptor.jsonShape;
      for (const part of parts) {
        if (shape?.kind === "nullable") shape = shape.inner;
        if (shape?.kind === "any") continue;
        if (shape?.kind !== "object" || !Object.hasOwn(shape.fields, part)) return undefined;
        shape = shape.fields[part]!.shape;
      }
      if (!parts.length) return descriptor;
      let scalarShape = shape;
      if (scalarShape?.kind === "nullable") scalarShape = scalarShape.inner;
      return { scalar: scalarShape?.kind === "number" ? "float64" : scalarShape?.kind === "string" || scalarShape?.kind === "boolean" ? scalarShape.kind : "json", jsonShape: shape, parse: value => validateJson(value, shape) };
    }
    const descriptor = transformed ? { scalar: "float64", parse: (value: unknown) => { if (typeof value !== "number" || !Number.isFinite(value)) throw new TypeError("Expected finite number"); return value; } } : resolve(name);
    if (!descriptor) throw new TypeError(`Unknown field ${table}.${name}`);
    if (typeof inputOperator !== "string" || !operators.includes(inputOperator)) {
      throw new TypeError("Unsupported comparison operator");
    }
    let operator = inputOperator;
    if (operator.startsWith("not ") && operator !== "not in" && operator !== "not exists") return mark({ kind: "not", predicate: compare(input, operator.slice(4), value, options) });
    if (operator === "not exists") { operator = "exists"; value = false; }
    if (operator === "exists") {
      if (typeof value !== "boolean") throw new TypeError("Exists requires a boolean");
      return mark({ kind: "compare", field: name, operator, operand: { kind: "value", value } });
    }
    let shape = descriptor.jsonShape;
    if (shape?.kind === "nullable") shape = shape.inner;
    if (shape?.kind === "array" && ["contains", "containsAny", "containsAll", "containedBy"].includes(operator)) {
      if (options !== undefined) throw new TypeError("Array membership does not accept text options");
      const itemShape = shape.item;
      const encodeItem = (value: unknown) => descriptor.encodeElement ? descriptor.encodeElement(value) : validateJson(value, itemShape);
      const parsed = operator === "contains" ? encodeItem(value) : Array.isArray(value) ? Array.from(value, encodeItem) : (() => { throw new TypeError("Expected array membership operand list"); })();
      return mark({ kind: "compare", field: name, operator, operand: { kind: "value", value: parsed } });
    }
    if (["containsAny", "containsAll", "containedBy"].includes(operator)) throw new TypeError("Array membership requires an array field");
    if (options !== undefined && !literalText.test(operator)) {
      throw new TypeError("Options require a literal text comparison");
    }
    if (value === undefined) throw new TypeError("Undefined predicate operand");
    if (value === null && (operator === "=" || operator === "!=")) {
      operator = operator === "=" ? "is" : "is not";
    }
    const parse = (item: unknown) => {
      const parsed = descriptor.encodeOperand ? descriptor.encodeOperand(item) : descriptor.encode ? descriptor.encode(item) : descriptor.parse(item);
      return typeof parsed === "bigint" ? parsed.toString() : parsed;
    };
    let operand: Comparison["operand"];
    const ref = fieldMetadata(value);
    const rhsTransformed = typeof value === "object" && value !== null && numericExpression in value;
    if (operator === "is" || operator === "is not") {
      if (value !== null) throw new TypeError("Null comparison requires null");
      operand = { kind: "value", value: null };
    } else if (rhsTransformed) {
      const rhs = value as unknown as { name: string | readonly string[]; table: string };
      if (rhs.table !== table || descriptor.scalar !== "float64" || !comparisonOperators.includes(operator)) throw new TypeError("Incompatible numeric expression comparison");
      operand = { kind: "field", field: rhs.name, transform: "arrayLength" };
    } else if (ref) {
      const rhs = resolve(fieldPath(value, table));
      if (!rhs || rhs.scalar !== descriptor.scalar || rhs.precision !== descriptor.precision || !comparisonOperators.includes(operator)) {
        throw new TypeError("Incompatible field comparison");
      }
      operand = { kind: "field", field: fieldPath(value, table) };
    } else if (listOperators.includes(operator)) {
      const range = rangeOperators.includes(operator);
      if (!Array.isArray(value) || (range && value.length !== 2)) {
        throw new TypeError("Expected predicate operand array");
      }
      const values = Array.from(value, item => {
        if (item === null && range) throw new TypeError("Range bounds cannot be null");
        return item === null ? null : parse(item);
      });
      operand = { kind: "value", value: values };
    } else if (textOperator.test(operator)) {
      if (descriptor.scalar !== "string" || typeof value !== "string") {
        throw new TypeError("Text comparison requires a string field and operand");
      }
      operand = { kind: "value", value };
    } else {
      if (value === null) throw new TypeError("Ordered comparison cannot use null");
      operand = { kind: "value", value: parse(value) };
    }
    if (
      [">", ">=", "<", "<=", ...rangeOperators].includes(operator) &&
      !["string", "int64", "float64", "decimal", "date", "timestamp", "localTimestamp"].includes(descriptor.scalar)
    ) {
      throw new TypeError("Boolean fields do not support ordered comparisons");
    }
    return mark({
      kind: "compare", field: name, operator, operand,
      ...(transformed ? { transform: "arrayLength" as const } : {}),
      ...(literalText.test(operator) ? { caseSensitive: options?.caseSensitive ?? true } : {}),
    });
  }

  function valid(predicate: unknown): Predicate {
    if (!predicate || typeof predicate !== "object") throw new TypeError("Expected predicate");
    if (predicateTables.get(predicate) !== table) {
      throw new TypeError("Expected predicate from the selected table");
    }
    // All branded predicates and their children are immutable, so they can be
    // reused without copying or recursively accepting mutable callback output.
    return predicate as Predicate;
  }

  const helpers: Record<string, unknown> = {};
  const helperOperators = {
    eq: "=", ne: "!=", gt: ">", gte: ">=", lt: "<", lte: "<=",
    inArray: "in", notInArray: "not in", between: "between",
    containsAny: "containsAny", containsAll: "containsAll", containedBy: "containedBy",
    contains: "contains", startsWith: "startsWith", endsWith: "endsWith",
    like: "like", ilike: "ilike",
  };
  for (const [name, operator] of Object.entries(helperOperators)) {
    helpers[name] = (field: unknown, value: unknown, options?: TextOptions) =>
      compare(field, operator, value, options);
  }
  helpers.exists = (field: unknown, present = true) => compare(field, "exists", present);
  helpers.arrayLength = (input: unknown) => {
    const name = fieldPath(input, table);
    // Reuse operand validation so non-array paths are rejected at construction.
    compare(input, "containsAll", []);
    return Object.freeze({ [numericExpression]: true, name, table });
  };
  helpers.isNull = (field: unknown) => compare(field, "is", null);
  helpers.isNotNull = (field: unknown) => compare(field, "is not", null);
  for (const kind of ["and", "or"] as const) {
    helpers[kind] = (...predicates: unknown[]) => mark(
      predicates.length
        ? { kind, predicates: predicates.map(valid) }
        : { kind: "constant", value: kind === "and" },
    );
  }
  helpers.not = (predicate: unknown) => mark({ kind: "not", predicate: valid(predicate) });

  return (args: readonly unknown[]): Predicate[] => {
    if (args.length === 2 && (args[1] === "exists" || args[1] === "not exists")) return [compare(args[0], args[1], true)];
    if (args.length === 3 || args.length === 4) {
      return [compare(args[0], args[1], args[2], args[3] as TextOptions | undefined)];
    }
    if (args.length !== 1) throw new TypeError("Expected where predicate");
    const value = args[0];
    if (value === true) return [mark({ kind: "constant", value: true })];
    if (typeof value === "function") return [valid(value(helpers))];
    if (!value || typeof value !== "object" || Array.isArray(value) || !Object.keys(value).length) {
      throw new TypeError("An empty filter is ambiguous; use where(true) to select every row");
    }
    return Object.entries(value).map(([name, item]) => compare(name, "=", item));
  };
}
