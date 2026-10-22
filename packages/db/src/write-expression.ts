import type { CompiledTable, TableShape } from "./schema.ts";
import { DatabaseError } from "./error.ts";

const expressionType: unique symbol = Symbol("write expression type");
const storedExpressionType: unique symbol = Symbol("write expression storage type");
const expressionTokens = new WeakSet<object>();
const policyType: unique symbol = Symbol("conflict policy type");
export interface WriteValue<V, B = V> { readonly [expressionType]: V; readonly [storedExpressionType]: B }
export type WriteReference<V, B = V, Stored = B> = WriteValue<V, Stored> & (NonNullable<B> extends number | bigint ? {
  add(value: NonNullable<B> | WriteValue<unknown, NonNullable<Stored>>): WriteReference<V, B, Stored>;
  add(value: WriteValue<unknown, NonNullable<Stored> | null>): WriteReference<NonNullable<V> | null, NonNullable<B> | null, NonNullable<Stored> | null>;
} : {});
export type WriteReferences<R, B = R, Stored = B> = { readonly [K in keyof R]: WriteReference<R[K], K extends keyof B ? B[K] : R[K], K extends keyof Stored ? Stored[K] : R[K]> };
export type WritePatch<P, B = P> = { [K in keyof P]?: P[K] | (Exclude<P[K], undefined> extends never ? never : WriteValue<unknown, Exclude<K extends keyof B ? B[K] : P[K], undefined>>) };
export type UpdateInput<R, P, B = R, Stored = B> = P | ((current: WriteReferences<R, B, Stored>) => WritePatch<P, Stored>);
export interface ConflictPolicy<Skip extends boolean = boolean> { readonly [policyType]: Skip }
export interface ConflictTarget<R, P, B = R, Stored = B> {
  skip(): ConflictPolicy<true>;
  update(patch: WritePatch<P, Stored> | ((rows: { readonly current: WriteReferences<R, B, Stored>; readonly incoming: WriteReferences<R, B, Stored> }) => WritePatch<P, Stored>)): ConflictPolicy<false>;
}
export interface ConflictSelector<R, P, Targets extends readonly string[], B = R, Stored = B> {
  on(...fields: Targets): ConflictTarget<R, P, B, Stored>;
}
type Primary<D extends TableShape> = D["options"] extends { readonly primaryKey: infer K extends string } ? readonly [K] : readonly ["id"];
type Unique<D extends TableShape> = D["options"] extends { readonly unique: infer U } ? { [K in keyof U]: U[K] extends { readonly fields: infer F extends readonly string[] } ? F : never }[keyof U] : never;
export type ConflictTargets<D extends TableShape> = Primary<D> | Unique<D>;
export type RowExpectation = number | ((count: number) => boolean);
export type WriteExpression =
  | { readonly kind: "value"; readonly value: unknown }
  | { readonly kind: "field"; readonly source: "current" | "incoming"; readonly field: string }
  | { readonly kind: "add"; readonly left: WriteExpression; readonly right: WriteExpression };
export interface CompiledConflict {
  readonly fields: readonly string[];
  readonly action: { readonly kind: "skip" } | { readonly kind: "update"; readonly values: Readonly<Record<string, WriteExpression>> };
}

/** A row expectation is evaluated by the transaction transport before commit. */
export function rowExpectation(input: unknown): (result: unknown) => void {
  if (typeof input !== "function" && (!Number.isSafeInteger(input) || (input as number) < 0)) throw new TypeError("Expected a nonnegative safe row count or synchronous predicate");
  return result => {
    const count = result === null ? 0 : Array.isArray(result) ? result.length : 1;
    const accepted = typeof input === "function" ? input(count) : count === input;
    if (typeof accepted !== "boolean") {
      // Observe a rejected promise from an accidentally async callback without accepting it.
      if (accepted && typeof accepted.then === "function") Promise.resolve(accepted).catch(() => {});
      throw new TypeError("Row expectations must return a synchronous boolean");
    }
    if (!accepted) throw new DatabaseError({ code: "row_expectation_failed", message: `Affected-row expectation failed (${count} rows)` });
  };
}

export function writeCompiler(table: CompiledTable) {
  type Meta = { node: WriteExpression; field: CompiledTable["fields"][string]; scope: object; depth: number; size: number; nullable: boolean };
  const nodes = new WeakMap<object, Meta>();
  function references(source: "current" | "incoming", scope: object) {
    return Object.freeze(Object.fromEntries(Object.entries(table.fields).map(([name, field]) => [name,
      reference({ kind: "field", source, field: name }, field, scope, 0, 1),
    ])));
  }
  function reference(node: WriteExpression, field: Meta["field"], scope: object, depth: number, size: number, nullable = field.allowsNull): object {
    const value = Object.create(null) as Record<string, unknown>;
    if (field.scalar === "int64" || field.scalar === "float64") {
      value.add = (right: unknown) => {
        const operand = expression(right, field, scope, true, true);
        if (Math.max(depth, operand.depth) + 1 > 64) throw new TypeError("Write expression nesting exceeds budget");
        if (size + operand.size + 1 > 1024) throw new TypeError("Write expression size exceeds budget");
        return reference(Object.freeze({ kind: "add", left: node, right: operand.node }), field,
          scope, Math.max(depth, operand.depth) + 1, size + operand.size + 1, nullable || operand.nullable);
      };
    }
    nodes.set(value, { node, field, scope, depth, size, nullable });
    expressionTokens.add(value);
    return Object.freeze(value);
  }
  function expression(value: unknown, field: Meta["field"], scope: object, allowNullReference = false, arithmetic = false): Meta {
    const meta = typeof value === "object" && value !== null ? nodes.get(value) : undefined;
    if (meta) {
      if (meta.scope !== scope) throw new TypeError("Write expressions belong to their declaration callback");
      if (meta.field.scalar !== field.scalar || meta.field.precision !== field.precision || (meta.nullable && !field.allowsNull && !allowNullReference)) throw new TypeError("Incompatible write expression assignment");
      return meta;
    }
    if (typeof value === "object" && value !== null && expressionTokens.has(value)) throw new TypeError("Write expressions belong to their declaration callback");
    let parsed: unknown;
    if (arithmetic) {
      if (field.scalar === "int64" && field.mode !== "bigint") {
        if (typeof value !== "number" || !Number.isSafeInteger(value)) throw new TypeError("int64 arithmetic requires a safe integer in number mode");
        value = BigInt(value);
      }
      parsed = field.storage(value);
    } else parsed = field.encode(value);
    return { node: Object.freeze({ kind: "value", value: typeof parsed === "bigint" ? parsed.toString() : parsed }), field, scope, depth: 0, size: 1, nullable: parsed === null };
  }
  function patch(input: unknown, scope: object): Record<string, WriteExpression> {
    if (typeof input !== "object" || input === null || Array.isArray(input) || ![Object.prototype, null].includes(Object.getPrototypeOf(input))) throw new TypeError("Expected a write patch object");
    let size = 0;
    return Object.freeze(Object.fromEntries(Object.entries(input).flatMap(([name, value]) => {
      if (!Object.hasOwn(table.fields, name)) throw new TypeError(`Unknown field ${table.name}.${name}`);
      if (name === table.primaryKey) throw new TypeError("Cannot update a primary key");
      if (value === undefined) return [];
      const item = expression(value, table.fields[name]!, scope);
      size += item.size;
      if (size > 1024) throw new TypeError("Write expression size exceeds budget");
      return [[name, item.node]];
    })));
  }
  return {
    update(input: unknown) {
      const scope = {};
      return patch(typeof input === "function" ? input(references("current", scope)) : input, scope);
    },
    conflict(callback: unknown): CompiledConflict {
      if (typeof callback !== "function") throw new TypeError("OnConflict requires a policy callback");
      const scope = {};
      const policies = new WeakMap<object, CompiledConflict>();
      const on = (...fields: unknown[]) => {
        if (!fields.length || fields.some(field => typeof field !== "string") || new Set(fields).size !== fields.length) throw new TypeError("Expected a complete primary or unique conflict key");
        const eligible = [[table.primaryKey], ...Object.values(table.unique).map(key => key.fields)];
        if (!eligible.some(key => key.length === fields.length && key.every(field => fields.includes(field)))) throw new TypeError("Conflict target must be a complete primary or unique key");
        const target = Object.freeze([...fields]) as readonly string[];
        const policy = (action: CompiledConflict["action"]) => {
          const token = Object.freeze(Object.create(null)) as object;
          policies.set(token, Object.freeze({ fields: target, action: Object.freeze(action) }));
          return token;
        };
        return Object.freeze({
          skip: () => policy({ kind: "skip" }),
          update: (input: unknown) => policy({ kind: "update", values: patch(typeof input === "function" ? input(Object.freeze({ current: references("current", scope), incoming: references("incoming", scope) })) : input, scope) }),
        });
      };
      const result: unknown = callback(Object.freeze({ on }));
      const selected = typeof result === "object" && result !== null ? policies.get(result) : undefined;
      if (!selected) throw new TypeError("Return a conflict policy from this callback");
      return selected;
    },
  };
}
