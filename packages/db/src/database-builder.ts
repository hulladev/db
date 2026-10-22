import type { DatabaseOperation, InsertQuery, PendingWrite, SelectQuery, WriteQuery } from "./database.ts";

import type { CompiledConflict, RowExpectation } from "./write-expression.ts";
import type { Page, PageOptions } from "./page.ts";
import type { Predicate } from "./filters.ts";
import type { AggregatePlan, GroupedAggregate } from "./aggregate.ts";
type Filter = Predicate;
type Order = { field: string; direction: "asc" | "desc" };
type ParseFilter = (args: readonly unknown[]) => Filter[];
type ParseProjection = (input: unknown) => readonly string[];

/** Private memoized execution shared by all database builders. */
class Operation<T> implements DatabaseOperation<T> {
  #result: Promise<T> | undefined;
  private readonly execute: () => Promise<T>;
  constructor(execute: () => Promise<T>) { this.execute = execute; }
  protected assertUnstarted() {
    if (this.#result) throw new TypeError("Cannot modify a database operation after execution starts");
  }
  private start(): Promise<T> {
    if (!this.#result) {
      try { this.#result = Promise.resolve(this.execute()); }
      catch (error) { this.#result = Promise.reject(error); }
    }
    return this.#result;
  }
  then<F = T, R = never>(fulfilled?: ((value: T) => F | PromiseLike<F>) | null, rejected?: ((reason: unknown) => R | PromiseLike<R>) | null): Promise<F | R> {
    return this.start().then(fulfilled, rejected);
  }
  catch<R = never>(rejected: (reason: unknown) => R | PromiseLike<R>): Promise<T | R> { return this.start().catch(rejected); }
  finally(finallyCallback: () => void): Promise<T> { return this.start().finally(finallyCallback); }
}

export function operation<T>(execute: () => Promise<T>): DatabaseOperation<T> { return new Operation(execute); }

type CheckRows = (result: unknown) => void;
type ParseExpectation = (input: RowExpectation) => CheckRows;
export function insertion<R, Bulk extends boolean>(
  parseProjection: ParseProjection,
  parseConflict: (input: unknown) => CompiledConflict,
  parseExpectation: ParseExpectation,
  execute: (projection?: readonly string[], conflict?: CompiledConflict, check?: CheckRows) => Promise<Bulk extends true ? R[] : R | null>,
): InsertQuery<R, Bulk> {
  class Insertion extends Operation<Bulk extends true ? R[] : R | null> {
    private readonly projection: readonly string[] | undefined;
    private readonly conflict: CompiledConflict | undefined;
    private readonly check: CheckRows | undefined;
    constructor(projection?: readonly string[], conflict?: CompiledConflict, check?: CheckRows) {
      super(() => execute(projection, conflict, check));
      this.projection = projection; this.conflict = conflict; this.check = check;
    }
    returning(input: unknown) {
      this.assertUnstarted();
      return new Insertion(parseProjection(input), this.conflict, this.check);
    }
    onConflict(input: unknown) {
      this.assertUnstarted();
      if (this.conflict) throw new TypeError("Only one conflict policy may be declared");
      return new Insertion(this.projection, parseConflict(input), this.check);
    }
    expectRows(input: RowExpectation) {
      this.assertUnstarted();
      if (this.check) throw new TypeError("Only one row expectation may be declared");
      return new Insertion(this.projection, this.conflict, parseExpectation(input));
    }
  }
  return new Insertion() as InsertQuery<R, Bulk>;
}

export function selection<R>(
  parse: ParseFilter,
  execute: (filters: Filter[], limit: number | null, unique: boolean, order: Order[]) => Promise<R[] | R | null>,
  parseOrder: (field: unknown) => string,
  paginate: (filters: Filter[], order: Order[], options: Required<PageOptions>) => Promise<Page<R>>,
  prepareAggregate: (groups: readonly unknown[], select: unknown) => AggregatePlan,
  executeAggregate: (filters: Filter[], plan: AggregatePlan, having: Filter[], order: Order[], limit: number | null) => Promise<unknown>,
): SelectQuery<R> {
  function grouped(filters: Filter[], plan: AggregatePlan): GroupedAggregate<Record<string, unknown>> {
    class Grouped extends Operation<Record<string, unknown>[]> implements GroupedAggregate<Record<string, unknown>> {
      private readonly havingFilters: Filter[];
      private readonly order: Order[];
      constructor(havingFilters: Filter[] = [], order: Order[] = []) {
        super(async () => await executeAggregate(filters, plan, havingFilters, order, null) as Record<string, unknown>[]);
        this.havingFilters = havingFilters;
        this.order = order;
      }
      having: GroupedAggregate<Record<string, unknown>>["having"] = predicate => {
        this.assertUnstarted();
        if (typeof predicate !== "function") throw new TypeError("Having requires a predicate callback");
        return new Grouped([...this.havingFilters, ...plan.parseHaving([predicate])], this.order);
      };
      orderBy(field: string, direction: "asc" | "desc" = "asc"): GroupedAggregate<Record<string, unknown>> {
        this.assertUnstarted();
        if (direction !== "asc" && direction !== "desc") throw new TypeError("Invalid order direction");
        return new Grouped(this.havingFilters, [...this.order, { field: plan.parseOrder(field), direction }]);
      }
      limit(count: number): DatabaseOperation<Record<string, unknown>[]> {
        this.assertUnstarted();
        if (!Number.isSafeInteger(count) || count < 1) throw new TypeError("Group limit must be a positive safe integer");
        return operation(async () => await executeAggregate(filters, plan, this.havingFilters, this.order, count) as Record<string, unknown>[]);
      }
    }
    return new Grouped();
  }
  class Selection extends Operation<R[]> implements SelectQuery<R> {
    private readonly filters: Filter[];
    private readonly order: Order[];
    constructor(filters: Filter[] = [], order: Order[] = []) {
      super(async () => await execute(filters, null, false, order) as R[]);
      this.filters = filters;
      this.order = order;
    }
    where(...args: unknown[]): SelectQuery<R> {
      this.assertUnstarted();
      return new Selection([...this.filters, ...parse(args)], this.order);
    }
    orderBy(field: unknown, direction: "asc" | "desc" = "asc"): SelectQuery<R> {
      this.assertUnstarted();
      if (direction !== "asc" && direction !== "desc") throw new TypeError("Invalid order direction");
      return new Selection(this.filters, [...this.order, { field: parseOrder(field), direction }]);
    }
    limit(count: number): DatabaseOperation<R[]> {
      this.assertUnstarted();
      if (!Number.isSafeInteger(count) || count < 1) throw new TypeError("Query limit must be a positive safe integer");
      return operation(async () => await execute(this.filters, count, false, this.order) as R[]);
    }
    paginate(options: PageOptions): DatabaseOperation<Page<R>> {
      this.assertUnstarted();
      if (!options || !Number.isSafeInteger(options.limit) || options.limit < 1 || options.limit > 1000) throw new TypeError("Page limit must be a positive safe integer at most 1000");
      if (options.cursor !== undefined && options.cursor !== null && typeof options.cursor !== "string") throw new TypeError("Page cursor must be a string or null");
      const parsed = { limit: options.limit, cursor: options.cursor ?? null };
      return operation(() => paginate(this.filters, this.order, parsed));
    }
    unique(): DatabaseOperation<R | null> {
      this.assertUnstarted();
      return operation(async () => await execute(this.filters, 2, true, this.order) as R | null);
    }
    private assertAggregate() {
      this.assertUnstarted();
      if (this.order.length) throw new TypeError("Apply ordering after grouped aggregate(), not to its input rows");
    }
    aggregate: SelectQuery<R>["aggregate"] = select => {
      this.assertAggregate();
      const plan = prepareAggregate([], select);
      return operation(() => executeAggregate(this.filters, plan, [], [], null)) as never;
    };
    groupBy: SelectQuery<R>["groupBy"] = (...groups) => {
      this.assertAggregate();
      if (!groups.length) throw new TypeError("GroupBy requires at least one field");
      return Object.freeze({
        aggregate: (select: unknown) => {
          this.assertAggregate();
          return grouped(this.filters, prepareAggregate(groups, select));
        },
      }) as never;
    };
  }
  return new Selection();
}

export function write<R>(parse: ParseFilter, parseProjection: ParseProjection, parseExpectation: ParseExpectation, execute: (filters: Filter[], projection?: readonly string[], check?: CheckRows) => Promise<R[]>): PendingWrite<R> {
  class Write extends Operation<R[]> implements WriteQuery<R> {
    private readonly filters: Filter[];
    private readonly scoped: boolean;
    private readonly projection: readonly string[] | undefined;
    private readonly check: CheckRows | undefined;
    constructor(filters: Filter[] = [], scoped = false, projection?: readonly string[], check?: CheckRows) {
      super(async () => {
        if (!scoped) throw new TypeError("Update and delete require where(); use where(true) to select every row");
        return execute(filters, projection, check);
      });
      this.filters = filters;
      this.scoped = scoped;
      this.projection = projection;
      this.check = check;
    }
    where(...args: unknown[]): WriteQuery<R> {
      this.assertUnstarted();
      return new Write([...this.filters, ...parse(args)], true, this.projection, this.check);
    }
    expectRows(input: RowExpectation): WriteQuery<R> {
      this.assertUnstarted();
      if (this.check) throw new TypeError("Only one row expectation may be declared");
      return new Write(this.filters, this.scoped, this.projection, parseExpectation(input));
    }
    returning: WriteQuery<R>["returning"] = input => {
      this.assertUnstarted();
      return new Write(this.filters, this.scoped, parseProjection(input), this.check) as never;
    };
  }
  return new Write() as unknown as PendingWrite<R>;
}
