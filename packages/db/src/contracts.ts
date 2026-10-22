export type FunctionKind = "query" | "mutation";

/** Signature metadata is type-only; browser modules never import handlers. */
export interface FunctionSignature {
  readonly kind: FunctionKind;
  readonly $types?: { readonly input: unknown; readonly output: unknown };
}

/** Shared type-only metadata for generated query and mutation callables. */
export declare const functionSignature: unique symbol;
export interface FunctionReference<D extends FunctionSignature = FunctionSignature> {
  readonly [functionSignature]?: D;
}

type DefinitionOf<T extends FunctionSignature | FunctionReference> = T extends FunctionSignature ? T
  : T extends FunctionReference<infer D> ? D : never;

/** Submitted input, before schema transforms. No-input functions use undefined. */
export type Input<T extends FunctionSignature | FunctionReference> = NonNullable<DefinitionOf<T>["$types"]>["input"];
/** Public result after schema transforms; query results are shared and readonly. */
export type Output<T extends FunctionSignature | FunctionReference> = DefinitionOf<T>["kind"] extends "query"
  ? ReadonlyResult<NonNullable<DefinitionOf<T>["$types"]>["output"]>
  : NonNullable<DefinitionOf<T>["$types"]>["output"];

/** Ordinary query result objects and arrays are shared, and must be copied before editing.
 * Built-ins such as Date retain their methods; readonly properties do not freeze their internals. */
export type ReadonlyResult<T> = T extends string | number | bigint | boolean | symbol | null | undefined | Date | ((...args: never[]) => unknown)
  ? T
  : T extends object ? { readonly [K in keyof T]: ReadonlyResult<T[K]> } : T;

export interface Operation {
  readonly name: string;
  readonly kind: FunctionKind;
  readonly input: unknown;
  readonly codecs?: import("./codec.ts").FunctionCodecs;
  /** Internal pagination ownership; range is learned from a complete live round. */
  readonly page?: { readonly owner: string; range?: string };
}

/** Internal adapter options. Mutation signals are deliberately absent from generated bindings. */
export interface CallOptions {
  readonly signal?: AbortSignal;
  readonly optimistic?: import("./optimistic.ts").Predictor;
}
export type QueryOptions = Pick<CallOptions, "signal">;
export interface QuerySubscriptionOptions extends QueryOptions {
  readonly keepPreviousDataOnError?: boolean;
}

type QueryIndicators = {
  readonly isStale?: boolean;
  readonly isOptimistic?: boolean;
};
export type QueryState<T> =
  | ({ readonly status: "pending"; readonly data?: ReadonlyResult<T> } & QueryIndicators)
  | { readonly status: "success"; readonly data: ReadonlyResult<T>; readonly isStale: boolean; readonly isOptimistic: boolean }
  | ({ readonly status: "error"; readonly error: unknown; readonly data?: ReadonlyResult<T> } & QueryIndicators);

export type MutationState<T> =
  | { readonly status: "pending" | "recovering" }
  | { readonly status: "success"; readonly data: T }
  | { readonly status: "error"; readonly error: unknown };

export interface QueryHandle<T> extends PromiseLike<ReadonlyResult<T>> {
  readonly status: QueryState<T>["status"];
  readonly data: ReadonlyResult<T> | undefined;
  readonly error: unknown;
  readonly isStale: boolean;
  readonly isOptimistic: boolean;
  subscribe(listener: (state: QueryState<T>) => void, options?: QuerySubscriptionOptions): () => void;
}

export interface MutationHandle<T> extends PromiseLike<T> {
  readonly status: MutationState<T>["status"];
  readonly data: T | undefined;
  readonly error: unknown;
  subscribe(listener: (state: MutationState<T>) => void): () => void;
}

/** Adapter boundary. The host, not a callable wrapper, owns retries and state. */
export interface FunctionRuntime {
  /** Local cache context for UI bindings, never proof of server identity. */
  readonly clientAuth?: { readonly key: string | null; readonly pending: boolean; readonly error?: unknown };
  /** Changes whenever an adapter retires its authorization/cache scope. */
  readonly identityEpoch?: unknown;
  /** Aborts when the current cache/authorization scope is retired. */
  readonly identitySignal?: AbortSignal;
  /** App-owned auth notifications; never a credential or proof of identity. */
  setAuth?(state: import("./auth.ts").ClientAuthInput): void | Promise<void>;
  call(operation: Operation, options: CallOptions, onStatus?: (status: "recovering") => void): Promise<unknown>;
  watch?(
    operation: Operation & { readonly kind: "query" },
    observer: (state: QueryState<unknown>) => void,
    options: QueryOptions,
  ): () => void;
}

export interface PageHandle<T> extends QueryHandle<readonly T[]> {
  readonly hasMore: boolean;
  next(pages?: number): Promise<void>;
  dispose(): void;
}
