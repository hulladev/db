import type { Page, PageOptions } from "./page.ts";
import {
  compileSchema, prepareInsert,
  type SchemaDefinition, type Row, type StorageRow, type StorageIdentity, type Insert, type Patch,
} from "./schema.ts";
import { tableName, fieldPath, type TableReference, type FieldReference } from "./fields.ts";
import { predicateParser, type Expressions, type Predicate, type Field, type FieldValue, type Paths } from "./filters.ts";
import { DatabaseError } from "./error.ts";
import { writeCompiler, rowExpectation, type RowExpectation, type ConflictSelector, type ConflictPolicy, type ConflictTargets, type UpdateInput } from "./write-expression.ts";
export type { RowExpectation, ConflictSelector, ConflictPolicy, ConflictTargets, UpdateInput, WriteReferences, WriteReference, WriteValue, WritePatch } from "./write-expression.ts";
import { insertion, selection, write } from "./database-builder.ts";
import { parseProjection, type Projection, type Projected, type ValidProjection } from "./projection.ts";
import { aggregatePlan, type Aggregates, type AggregateSelection, type AggregateResult, type GroupedQuery, type GroupKey } from "./aggregate.ts";
export type { Aggregates, AggregateValue, AggregateResult, GroupedQuery, GroupedAggregate } from "./aggregate.ts";

type Tables<S extends SchemaDefinition> = Extract<keyof S["tables"], string>;
export interface DatabaseOperation<T> extends PromiseLike<T> {
  catch<TResult = never>(onRejected: (reason: unknown) => TResult | PromiseLike<TResult>): Promise<T | TResult>;
  finally(onFinally: () => void): Promise<T>;
}
type TableInput<S extends SchemaDefinition, T extends Tables<S>> = T | TableReference<S, T>;
type LiteralTextOperator =
  | "contains" | "not contains" | "startsWith" | "not startsWith"
  | "endsWith" | "not endsWith";
type TextOperator = LiteralTextOperator | "like" | "not like" | "ilike" | "not ilike";
type Operator<V> =
  | "=" | "!=" | "in" | "not in" | "is" | "is not" | "exists" | "not exists"
  | (NonNullable<V> extends readonly unknown[] ? "contains" | "not contains" | "containsAny" | "not containsAny" | "containsAll" | "not containsAll" | "containedBy" | "not containedBy" : never)
  | (NonNullable<V> extends string | number | bigint | Date ? ">" | ">=" | "<" | "<=" | "between" | "not between" : never)
  | (NonNullable<V> extends string ? TextOperator : never);
type Operand<V, O, T extends string> =
  O extends "exists" | "not exists" ? boolean
  : O extends "contains" | "not contains" ? NonNullable<V> extends readonly (infer E)[] ? E : string
  : O extends "containsAny" | "not containsAny" | "containsAll" | "not containsAll" | "containedBy" | "not containedBy" ? NonNullable<V> extends readonly (infer E)[] ? readonly E[] : never
  : O extends "is" | "is not" ? null
  : O extends "in" | "not in" ? readonly (V | null)[]
  : O extends "between" | "not between" ? readonly [NonNullable<V>, NonNullable<V>]
  : O extends TextOperator ? string
  : O extends "=" | "!=" ? Exclude<V, undefined> | null | FieldReference<V | null, T>
  : NonNullable<V> | FieldReference<V | null, T>;

export interface Where<R, Result, T extends string = string, B = R> {
  where(filter: Partial<{ [P in Paths<B>]: Exclude<FieldValue<R, P>, undefined> | null }> | true | ((expressions: Expressions<R, T, B>) => Predicate)): Result;
  where(field: Field<R, T, B>, operator: "exists" | "not exists"): Result;
  where<F extends Field<R, T, B>, O extends Operator<F extends FieldReference<unknown, string, infer V> ? V : FieldValue<B, NoInfer<F>>>>(
    field: F,
    operator: O,
    value: Operand<FieldValue<R, NoInfer<F>>, NoInfer<O>, T>,
    options?: O extends LiteralTextOperator ? NonNullable<F extends FieldReference<unknown, string, infer V> ? V : FieldValue<B,F>> extends string ? { caseSensitive?: boolean } : never : never,
  ): Result;
}
export interface SelectQuery<R, T extends string = string, O = R, B = R>
  extends DatabaseOperation<O[]>, Where<R, SelectQuery<R, T, O, B>, T, B> {
  orderBy(field: Field<R, T, B>, direction?: "asc" | "desc"): SelectQuery<R, T, O, B>;
  limit(count: number): DatabaseOperation<O[]>;
  unique(): DatabaseOperation<O | null>;
  paginate(options: PageOptions): DatabaseOperation<Page<O>>;
  aggregate<A extends AggregateSelection>(select: (aggregates: Aggregates<R, T, B>) => A): DatabaseOperation<AggregateResult<A>>;
  groupBy<const G extends readonly [GroupKey<B> & keyof R, ...(GroupKey<B> & keyof R)[]]>(...fields: G): GroupedQuery<R, G[number], T, B>;
}
export interface WriteQuery<R, T extends string = string, O = R, B = R>
  extends DatabaseOperation<O[]>, Where<R, WriteQuery<R, T, O, B>, T, B> {
  returning<const P extends Projection<R>>(projection: P & ValidProjection<R, P>): WriteQuery<R, T, Projected<R, P>, B>;
  expectRows(expectation: RowExpectation): WriteQuery<R, T, O, B>;
}
export interface PendingWrite<R, T extends string = string, O = R, B = R>
  extends Where<R, WriteQuery<R, T, O, B>, T, B> {
  returning<const P extends Projection<R>>(projection: P & ValidProjection<R, P>): PendingWrite<R, T, Projected<R, P>, B>;
  expectRows(expectation: RowExpectation): PendingWrite<R, T, O, B>;
}
export interface InsertQuery<R, Bulk extends boolean = false, O = R, Targets extends readonly string[] = readonly string[], P = Partial<R>, Skip extends boolean = false, B = R, Stored = B> extends DatabaseOperation<Bulk extends true ? O[] : Skip extends true ? O | null : O> {
  returning<const Selection extends Projection<R>>(projection: Selection & ValidProjection<R, Selection>): InsertQuery<R, Bulk, Projected<R, Selection>, Targets, P, Skip, B, Stored>;
  onConflict<S extends boolean>(policy: (selector: ConflictSelector<R, P, Targets, B, Stored>) => ConflictPolicy<S>): InsertQuery<R, Bulk, O, Targets, P, S, B, Stored>;
  expectRows(expectation: RowExpectation): InsertQuery<R, Bulk, O, Targets, P, Skip, B, Stored>;
}
export interface ReadDatabase<S extends SchemaDefinition> {
  select<T extends Tables<S>>(table: TableInput<S, T>): SelectQuery<Row<S, T>, T, Row<S, T>, StorageRow<S, T>>;
  select<T extends Tables<S>, const P extends Projection<Row<S, NoInfer<T>>>>(
    table: TableInput<S, T>, projection: P & ValidProjection<Row<S, NoInfer<T>>, P>,
  ): SelectQuery<Row<S, T>, T, Projected<Row<S, T>, P>, StorageRow<S, T>>;
}
export interface WriteDatabase<S extends SchemaDefinition> extends ReadDatabase<S> {
  insert<T extends Tables<S>>(
    table: TableInput<S, T>, input: Insert<S, NoInfer<T>>,
  ): InsertQuery<Row<S, T>, false, Row<S, T>, ConflictTargets<S["tables"][T]>, Patch<S, T>, false, StorageRow<S, T>, StorageIdentity<S, T>>;
  insert<T extends Tables<S>>(
    table: TableInput<S, T>, input: readonly Insert<S, NoInfer<T>>[],
  ): InsertQuery<Row<S, T>, true, Row<S, T>, ConflictTargets<S["tables"][T]>, Patch<S, T>, false, StorageRow<S, T>, StorageIdentity<S, T>>;
  update<T extends Tables<S>>(
    table: TableInput<S, T>, patch: UpdateInput<Row<S, NoInfer<T>>, Patch<S, NoInfer<T>>, StorageRow<S, NoInfer<T>>, StorageIdentity<S, NoInfer<T>>>,
  ): PendingWrite<Row<S, T>, T, Row<S, T>, StorageRow<S, T>>;
  delete<T extends Tables<S>>(table: TableInput<S, T>): PendingWrite<Row<S, T>, T, Row<S, T>, StorageRow<S, T>>;
}
export interface QueryContext<S extends SchemaDefinition, I = unknown> {
  readonly db: ReadDatabase<S>;
  readonly identity: I;
}
export interface MutationContext<S extends SchemaDefinition, I = unknown> {
  readonly db: WriteDatabase<S>;
  readonly identity: I;
}

/** Private transaction transport. Applications receive typed databases, never this channel. */
export function transactionDatabase<S extends SchemaDefinition>(
  schema: S, writable: boolean, request: (operation: Record<string, unknown>, validate?: (result: unknown) => void) => Promise<unknown>,
): ReadDatabase<S> | WriteDatabase<S> {
  const tables = new Map(compileSchema(schema).map(table => [table.name, table]));
  function table(name: string) {
    const value = tables.get(name);
    if (!value) throw new TypeError(`Unknown table ${name}`);
    return value;
  }
  function encode(name: string, values: unknown) {
    const fields = table(name).fields;
    if (typeof values !== "object" || values === null || Array.isArray(values)) throw new TypeError("Expected record");
    return Object.fromEntries(Object.entries(values).map(([key, value]) => {
      const field = fields[key];
      if (!field) throw new TypeError(`Unknown field ${name}.${key}`);
      const parsed = field.encode(value);
      return [key, typeof parsed === "bigint" ? parsed.toString() : parsed];
    }));
  }
  function projection(name: string, input: unknown) {
    return parseProjection(table(name).fields, input);
  }
  function stored(name: string, value: unknown, selected?: readonly string[]): Row<S, Tables<S>> {
    if (typeof value !== "object" || value === null || Array.isArray(value)) throw new TypeError("Invalid stored row");
    const envelope = value as { row: Record<string, unknown>; version: unknown };
    if (typeof envelope.version !== "string" || !/^[1-9][0-9]*$/.test(envelope.version) || BigInt(envelope.version) >= 1n << 63n) throw new TypeError("Invalid storage version");
    if (typeof envelope.row !== "object" || envelope.row === null || Array.isArray(envelope.row)) throw new TypeError("Invalid stored row");
    const fields = table(name).fields;
    const selectedFields = selected ?? Object.keys(fields);
    if (Object.keys(envelope.row).length !== selectedFields.length || selectedFields.some(key => !Object.hasOwn(envelope.row, key))) throw new TypeError("Stored row does not match requested projection");
    return Object.fromEntries(selectedFields.map(key => [key, fields[key]!.decode(envelope.row[key])])) as Row<S, Tables<S>>;
  }
  function rows(name: string, result: unknown, selected?: readonly string[]): Row<S, Tables<S>>[] {
    if (!Array.isArray(result)) throw new TypeError("Invalid database row result");
    return result.map(value => stored(name, value, selected));
  }
  function filter(name: string, args: readonly unknown[]): Predicate[] {
    return predicateParser(name, table(name).fields)(args);
  }
  const read: ReadDatabase<S> = {
    select(input: TableInput<S, Tables<S>>, inputProjection?: unknown) {
      const name = tableName(input);
      table(name);
      const selected = inputProjection === undefined ? undefined : projection(name, inputProjection);
      const projected = selected ? { projection: selected } : {};
      return selection(args => filter(name, args), async (filters, limit, unique, order) => {
        const result = rows(name, await request({ method: "select", table: name, filters, limit, order, ...projected }), selected);
        if (unique) {
          if (result.length > 1) throw new DatabaseError({ code: "not_unique", message: "Expected at most one matching row" });
          return result[0] ?? null;
        }
        return result;
      }, field => {
        const pathValue = fieldPath(field, name);
        if (typeof pathValue !== "string" && pathValue.length !== 1) throw new TypeError("JSON path ordering is not supported");
        const nameValue = typeof pathValue === "string" ? pathValue : pathValue[0]!;
        if (table(name).fields[nameValue]?.scalar === "json") throw new TypeError("JSON ordering is not supported");
        if (!table(name).fields[nameValue]) throw new TypeError(`Unknown field ${name}.${nameValue}`);
        return nameValue;
      }, async (filters, order, options) => {
        const result = await request({ method: "paginate", table: name, filters, order, ...options, ...projected });
        if (typeof result !== "object" || result === null || Array.isArray(result)) throw new TypeError("Invalid database page result");
        const page = result as { page: unknown; cursor: unknown; range: unknown };
        if ((page.cursor !== null && typeof page.cursor !== "string") || typeof page.range !== "string") throw new TypeError("Invalid database page result");
        return { page: rows(name, page.page, selected), cursor: page.cursor } as Page<Row<S, Tables<S>>>;
      }, (groups, select) => {
        if (selected) throw new TypeError("Aggregates select their own output; omit the row projection");
        return aggregatePlan(table(name), groups, select);
      }, async (filters, plan, having, order, limit) => {
        return plan.decode(await request({
          method: "aggregate", table: name, filters, groupBy: plan.groupBy,
          aggregates: plan.aggregates, having, order, limit,
        }));
      }) as never;
    },
  };
  if (!writable) return Object.freeze(read);
  return Object.freeze({
    ...read,
    insert(tableInput: TableInput<S, Tables<S>>, input: Insert<S, Tables<S>> | readonly Insert<S, Tables<S>>[]) {
      const name = tableName(tableInput) as Tables<S>;
      table(name);
      const bulk = Array.isArray(input);
      const inputs = bulk ? input : [input];
      const values = inputs.map(value => encode(name, prepareInsert(schema, name, value, false)));
      const compiler = writeCompiler(table(name));
      return insertion(input => projection(name, input), policy => compiler.conflict(policy), rowExpectation, async (selected, conflict, check) => {
        if (bulk && !values.length && !check) return [];
        const result = await request({ method: "insert", table: name, values: bulk ? values : values[0], ...(selected ? { projection: selected } : {}), ...(conflict ? { conflict } : {}) }, check);
        return bulk ? rows(name, result, selected) : result === null && conflict?.action.kind === "skip" ? null : stored(name, result, selected);
      });
    },
    update(input: Tables<S> | TableReference<S, Tables<S>>, patch: unknown) {
      const name = tableName(input);
      const values = writeCompiler(table(name)).update(patch);
      return write(args => filter(name, args), input => projection(name, input), rowExpectation, async (filters, selected, check) => rows(name,
        await request({ method: "update", table: name, filters, values, ...(selected ? { projection: selected } : {}) }, check), selected));
    },
    delete(input: Tables<S> | TableReference<S, Tables<S>>) {
      const name = tableName(input);
      table(name);
      return write(args => filter(name, args), input => projection(name, input), rowExpectation, async (filters, selected, check) => rows(name,
        await request({ method: "delete", table: name, filters, ...(selected ? { projection: selected } : {}) }, check), selected));
    },
  }) as unknown as WriteDatabase<S>;
}
