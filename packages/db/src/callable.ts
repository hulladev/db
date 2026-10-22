import { canonical } from "./http/protocol.ts";
import { PaginationOperation } from "./pagination.ts";
import type { Page } from "./page.ts";
import { stagedObserver } from "./notifications.ts";
import type {
  FunctionReference, FunctionSignature, Input, MutationHandle, Operation,
  functionSignature, PageHandle, QueryHandle, QueryState, QueryOptions, QuerySubscriptionOptions, ReadonlyResult,
} from "./contracts.ts";
import type { RuntimeScope } from "./runtime.ts";
import type { FunctionCodecs } from "./codec.ts";
import { DatabaseError } from "./error.ts";
import type { Predictor } from "./optimistic.ts";
import { MutationOperation, QueryOperation, queryListener, querySnapshot, subscriptionOptions } from "./handles.ts";

type CallArguments<I, Options> = [I] extends [undefined]
  ? [options?: Options]
  : [input: I, options?: Options];
type SubscribeArguments<I, O> = [I] extends [undefined]
  ? [listener: (state: QueryState<O>) => void, options?: QuerySubscriptionOptions]
  : [input: I, listener: (state: QueryState<O>) => void, options?: QuerySubscriptionOptions];

type ResultOf<D extends FunctionSignature> = NonNullable<D["$types"]>["output"];
const bindingMetadata = Symbol("hulla query binding");
export interface BindingInfo {
  readonly scope: RuntimeScope;
  readonly name: string;
  readonly kind: "query" | "mutation";
  readonly hasInput: boolean;
  readonly codecs?: FunctionCodecs;
}
/** Shared adapter boundary; never executes the callable or imports its handler. */
export function bindingInfo(reference: unknown): BindingInfo {
  const metadata = (reference as { [bindingMetadata]?: BindingInfo } | null)?.[bindingMetadata];
  if (!metadata) throw new TypeError("A framework binding requires a generated Hulla function");
  return metadata;
}
type QueryReference = ((...args: never[]) => QueryHandle<unknown>) & FunctionReference;
type DefinitionOf<Q extends QueryReference> = NonNullable<Q[typeof functionSignature]>;
type OptimisticSelection<I, QI> = {
  readonly input?: (input: I) => QI;
  readonly match?: (queryInput: QI, input: I) => boolean;
} & ([QI] extends [undefined] ? unknown :
  { readonly input: (input: I) => QI } | { readonly match: (queryInput: QI, input: I) => boolean });
export type OptimisticTarget<I, Q extends QueryReference> = {
  readonly query: Q;
  readonly update: (current: ReadonlyResult<ResultOf<DefinitionOf<Q>>>, input: I, queryInput: Input<DefinitionOf<Q>>) => ReadonlyResult<ResultOf<DefinitionOf<Q>>>;
} & OptimisticSelection<I, Input<DefinitionOf<Q>>>;
export type MutationCallable<D extends FunctionSignature> = ((...args: [Input<D>] extends [undefined] ? [] : [input: Input<D>]) => MutationHandle<ResultOf<D>>) & FunctionReference<D> & {
  withOptimistic<const Q extends readonly [QueryReference, ...QueryReference[]]>(...targets: { [K in keyof Q]: OptimisticTarget<Input<D>, Q[K]> }): MutationCallable<D>;
};
export type QueryCallable<D extends FunctionSignature> = ((...args: CallArguments<Input<D>, QueryOptions>) => QueryHandle<ResultOf<D>>) & FunctionReference<D> & {
  subscribe(...args: SubscribeArguments<Input<D>, ResultOf<D>>): () => void;
} & (ResultOf<D> extends Page<infer Row> ? { paginate(input: (cursor: string | null) => Input<D>): PageHandle<Row> } : {});

function options(value: unknown): QueryOptions {
  if (value === undefined) return {};
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new TypeError("Call options must be an object");
  if (Object.keys(value).some((key) => key !== "signal")) throw new TypeError("Unknown call option");
  const signal = (value as QueryOptions).signal;
  if (signal !== undefined && !(signal instanceof AbortSignal)) throw new TypeError("signal must be an AbortSignal");
  return signal === undefined ? {} : { signal };
}

function invocation(scope: RuntimeScope, name: string, kind: Operation["kind"], hasInput: boolean, args: unknown[], codecs?: FunctionCodecs) {
  if (args.length > (hasInput ? (kind === "query" ? 2 : 1) : (kind === "query" ? 1 : 0))) throw new TypeError("Too many function arguments");
  const configuration = kind === "query" ? options(args[hasInput ? 1 : 0]) : {};
  configuration.signal?.throwIfAborted();
  return {
    runtime: scope.current(),
    operation: { name, kind, input: hasInput ? args[0] : undefined, ...(codecs ? { codecs } : {}) },
    configuration,
  };
}

/** Used by generated modules; input/output types come from a type-only import. */
export function mutationBinding<D extends FunctionSignature & { readonly kind: "mutation" }>(
  scope: RuntimeScope, name: string, hasInput: boolean, codecs?: FunctionCodecs,
): MutationCallable<D> {
  const bind = (predictors: readonly Predictor[]): MutationCallable<D> => {
    const call = (...args: unknown[]) => {
      let context: ReturnType<typeof invocation> | undefined;
      let identityEpoch: unknown;
      let identitySignal: AbortSignal | undefined;
      return new MutationOperation<ResultOf<D>>((onStatus) => {
        context = invocation(scope, name, "mutation", hasInput, args, codecs);
        const { runtime, operation, configuration } = context;
        identityEpoch = runtime.identityEpoch;
        identitySignal = runtime.identitySignal;
        const optimistic: Predictor = (input, context) => { for (const predictor of predictors) predictor(input, context); };
        return runtime.call(operation, predictors.length ? { ...configuration, optimistic } : configuration, onStatus);
      }, codecs, () => {
        if (context && context.runtime.identityEpoch !== identityEpoch) throw new DatabaseError({ code: "context_changed", message: "Mutation authorization scope changed; the write may have committed" });
      }, () => identitySignal);
    };
    const withOptimistic = (...targets: readonly OptimisticTarget<Input<D>, QueryReference>[]) => {
      if (!targets.length) throw new TypeError("withOptimistic needs at least one query target");
      const additions: Predictor[] = targets.map(target => {
        const metadata = (target.query as QueryReference & { [bindingMetadata]?: BindingInfo })[bindingMetadata];
        if (!metadata || metadata.scope !== scope || metadata.kind !== "query" || (target.input !== undefined && typeof target.input !== "function") || typeof target.update !== "function" || (target.match !== undefined && typeof target.match !== "function")) {
          throw new TypeError("Optimistic targets need a generated query, updater and valid input/match callbacks");
        }
        const { input: mapInput, update, match } = target;
        if (!mapInput && !match && metadata.hasInput) {
          // A declaration can explicitly validate undefined. Its portable input
          // contract then permits the same exact undefined target as a no-input query.
          try {
            if (!metadata.codecs) throw new Error();
            metadata.codecs.input.parse(undefined);
          } catch { throw new TypeError("Optimistic targets for queries with input need an input mapper or match callback"); }
        }
        return (submitted, context) => {
          const input = submitted as Input<D>;
          const candidates = context.queries(metadata.name);
          const selected = match ? undefined : mapInput?.(input);
          const encode = (value: unknown) => canonical(metadata.codecs ? metadata.codecs.input.encode(value) : value);
          const exact = match ? undefined : encode(selected);
          const queries = candidates.filter(query => match ? match(query.input, input) : encode(query.input) === exact);
          for (const query of queries) {
            const value = context.get(query);
            if (value === undefined) continue;
            const state = querySnapshot({ status: "success", data: value, isStale: false, isOptimistic: false }, metadata.codecs);
            if (state.status === "error") throw state.error;
            if (state.status !== "success") continue;
            context.set(query, update(state.data, input, query.input));
          }
        };
      });
      return bind([...predictors, ...additions]);
    };
    return Object.assign(call, { withOptimistic, [bindingMetadata]: { name, scope, kind: "mutation", hasInput, ...(codecs ? { codecs } : {}) } }) as MutationCallable<D>;
  };
  return bind([]);
}

export function queryBinding<D extends FunctionSignature & { readonly kind: "query" }>(
  scope: RuntimeScope, name: string, hasInput: boolean, codecs?: FunctionCodecs, paginated = false,
): QueryCallable<D> {
  const call = (...args: unknown[]) => {
    let context: ReturnType<typeof invocation> | undefined;
    let identityEpoch: unknown;
    let identitySignal: AbortSignal | undefined;
    return new QueryOperation<ResultOf<D>>(() => {
      context = invocation(scope, name, "query", hasInput, args, codecs);
      identityEpoch = context.runtime.identityEpoch;
      identitySignal = context.runtime.identitySignal;
      return context.runtime.call(context.operation, context.configuration);
    }, (listener) => {
      if (!context) throw new Error("Cannot observe an invalid query invocation");
      const { runtime, operation, configuration } = context;
      if (!runtime.watch) throw new Error("The configured runtime does not support query subscriptions");
      return runtime.watch(operation as Operation & { kind: "query" },
        stagedObserver(state => listener(querySnapshot<ResultOf<D>>(state, codecs))), configuration);
    }, codecs, () => {
      if (context && context.runtime.identityEpoch !== identityEpoch) throw new DatabaseError({ code: "context_changed", message: "Query authorization scope changed" });
    }, () => context?.configuration.signal, () => identitySignal);
  };
  const subscribe = (...args: unknown[]) => {
    if (args.length > (hasInput ? 3 : 2)) throw new TypeError("Too many subscription arguments");
    const input = hasInput ? args[0] : undefined;
    const listener = args[hasInput ? 1 : 0];
    if (typeof listener !== "function") throw new TypeError("A query subscription needs a listener");
    const configuration = subscriptionOptions(args[hasInput ? 2 : 1]);
    configuration.signal?.throwIfAborted();
    const runtime = scope.current();
    if (!runtime.watch) throw new Error("The configured runtime does not support query subscriptions");
    const deliver = queryListener(listener as (state: QueryState<ResultOf<D>>) => void, configuration);
    return runtime.watch({ name, kind: "query", input, ...(codecs ? { codecs } : {}) },
      state => deliver(querySnapshot<ResultOf<D>>(state, codecs)),
      configuration.signal ? { signal: configuration.signal } : {});
  };
  return Object.assign(call, { subscribe, ...(paginated ? { paginate: (mapper: (cursor: string | null) => Input<D>) => new PaginationOperation(scope.current(), name, mapper, codecs) } : {}), [bindingMetadata]: { name, scope, kind: "query", hasInput, ...(codecs ? { codecs } : {}) } }) as unknown as QueryCallable<D>;
}
