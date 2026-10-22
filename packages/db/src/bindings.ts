/** Framework-neutral observation. UI packages own rendering, never a second cache. */
import type { FunctionReference, FunctionSignature, FunctionRuntime, Input, Output, QueryState, MutationState, MutationHandle, QueryHandle, PageHandle, ReadonlyResult } from "./contracts.ts";
import { bindingInfo } from "./callable.ts";
import { clearsPrevious, querySnapshot } from "./handles.ts";
import { canonical } from "./http/protocol.ts";
import { notification, stagedObserver } from "./notifications.ts";
import { DatabaseError } from "./error.ts";

export type QueryFunction = FunctionReference<FunctionSignature & { kind: "query" }>
  & ((...args: never[]) => QueryHandle<unknown>);
/** Only generated references carrying the direct-page capability qualify. */
export type PaginatedQueryFunction = QueryFunction & {
  paginate(mapper: (cursor: string | null) => never): PageHandle<unknown>;
};
export type PageRow<Q extends PaginatedQueryFunction> = Output<Q> extends { readonly page: readonly (infer Row)[] } ? Row : never;
export type PaginationBindingOptions<T> = QueryBindingOptions<readonly T[]>;
export type PaginationSnapshot<T> = QueryState<readonly T[]> & {
  readonly hasMore: boolean;
  readonly next: (pages?: number) => Promise<void>;
};
export type MutationFunction = FunctionReference<FunctionSignature & { kind: "mutation" }>
  & ((...args: never[]) => MutationHandle<unknown>);
export interface QueryBindingOptions<T> {
  /** Authorized SSR data, displayed only until the first live result or failure. */
  readonly initialData?: T;
  /** Same cache key supplied to setAuth on the server-rendered account. Defaults to anonymous. */
  readonly initialAuthKey?: string | null;
  readonly keepPreviousDataOnError?: boolean;
}
export type QueryArguments<Q extends QueryFunction> = [Input<Q>] extends [undefined]
  ? [options?: QueryBindingOptions<NoInfer<Output<Q>>>]
  : [input: Input<Q>, options?: QueryBindingOptions<NoInfer<Output<Q>>>];
export type MutationArguments<M extends MutationFunction> = [Input<M>] extends [undefined] ? [] : [input: Input<M>];
export type MutationBindingState<T> = { readonly status: "idle" } | MutationState<T>;

export function queryArguments<Q extends QueryFunction>(query: Q, args: QueryArguments<Q>) {
  const { hasInput } = bindingInfo(query);
  return {
    hasInput,
    input: (hasInput ? args[0] : undefined) as Input<Q>,
    options: ((hasInput ? args[1] : args[0]) ?? {}) as QueryBindingOptions<Output<Q>>,
  };
}
export function queryKey<Q extends QueryFunction>(query: Q, input: Input<Q>): string {
  const { codecs } = bindingInfo(query);
  const encoded = codecs ? codecs.input.encode(input) : input;
  return encoded === undefined ? "undefined" : canonical(encoded);
}

function emit(listeners: Set<() => void>): void {
  for (const listener of [...listeners]) {
    try { listener(); } catch { /* Observer failures belong to the application. */ }
  }
}

/** Shared auth, SSR seed and subscription lifetime for ordinary and paginated queries. */
function createObservation<T>(query: QueryFunction, options: QueryBindingOptions<T>,
  watch: (owner: FunctionRuntime, receive: (state: QueryState<T>) => void) => () => void) {
  const metadata = bindingInfo(query);
  if (metadata.kind !== "query") throw new TypeError("useQuery requires a query");
  if (options.initialAuthKey !== undefined && options.initialAuthKey !== null && typeof options.initialAuthKey !== "string") throw new TypeError("initialAuthKey must be a string or null");
  const pending: QueryState<T> = { status: "pending" };
  const initial: QueryState<T> = Object.hasOwn(options, "initialData")
    ? { status: "pending", data: options.initialData as ReadonlyResult<T>, isStale: true, isOptimistic: false } : pending;
  let state: QueryState<T> = initial;
  let seed = initial !== pending;
  let hasLiveData = false;
  let runtime: FunctionRuntime | undefined;
  let epoch: unknown;
  let needsAttach = true;
  let generation = 0;
  let stopWatch: (() => void) | undefined;
  let stopScope: (() => void) | undefined;
  let stopIdentity: (() => void) | undefined;
  const listeners = new Set<() => void>();
  const publish = (next: QueryState<T>) => {
    state = next;
    notification(() => emit(listeners));
  };
  const disconnect = () => {
    generation++;
    const stop = stopWatch; stopWatch = undefined; stop?.();
    stopIdentity?.(); stopIdentity = undefined;
  };
  const current = () => {
    try { return metadata.scope.current(); } catch { return undefined; }
  };
  const seedMatches = (owner: FunctionRuntime) => {
    const auth = owner.clientAuth;
    return !auth?.pending && !auth?.error && (auth?.key ?? null) === (options.initialAuthKey ?? null);
  };
  const attach = () => {
    const owner = current();
    if (!needsAttach && owner && owner === runtime && owner.identityEpoch === epoch) return;
    disconnect();
    needsAttach = false;
    if (runtime) { seed = false; hasLiveData = false; state = pending; }
    runtime = owner;
    epoch = owner?.identityEpoch;
    if (!owner) { publish({ status: "error", error: new Error("Configure the browser runtime before mounting Hulla bindings") }); return; }
    if (!seedMatches(owner)) { seed = false; state = pending; }
    const auth = owner.clientAuth;
    if (auth?.pending) { publish(pending); return; }
    if (auth?.error) { publish({ status: "error", error: auth.error }); return; }
    const signal = owner.identitySignal;
    const previousKey = owner.clientAuth?.key ?? null;
    const retired = () => {
      disconnect(); seed = false; hasLiveData = false;
      epoch = owner.identityEpoch;
      needsAttach = true;
      const reason: unknown = signal?.reason;
      publish(reason instanceof DatabaseError && reason.code !== "context_changed"
        ? { status: "error", error: reason } : pending);
      // Expiry changes the key without setAuth. Reattach only for a local key
      // change; a server authorization error must not create a retry loop.
      if ((owner.clientAuth?.key ?? null) !== previousKey) queueMicrotask(() => { if (listeners.size) attach(); });
    };
    signal?.addEventListener("abort", retired, { once: true });
    stopIdentity = () => signal?.removeEventListener("abort", retired);
    const active = generation;
    try {
      const stop = watch(owner, stagedObserver(raw => {
        if (active !== generation) return;
        let next = raw;
        if (next.status === "pending" && seed) next = initial;
        else if (next.status !== "pending") {
          seed = false;
          if (next.status === "error" && hasLiveData && options.keepPreviousDataOnError && !clearsPrevious(next.error) && state.data !== undefined) {
            next = { ...next, data: state.data, isStale: true, isOptimistic: false };
          }
          if (next.status === "success") hasLiveData = true;
        }
        publish(next);
      }));
      if (active === generation && listeners.size) stopWatch = stop;
      else stop();
    } catch (error) { seed = false; publish({ status: "error", error }); }
  };
  const getSnapshot = (): QueryState<T> => {
    // Also fence a seed before effects run, e.g. hydration after account switch.
    const owner = current();
    if (runtime && runtime.identityEpoch !== epoch) { seed = false; state = pending; }
    if (seed && owner && !seedMatches(owner)) { seed = false; state = pending; }
    return state;
  };
  return {
    getSnapshot,
    // SSR and hydration use the exact same supplied snapshot, never a shared
    // browser cache or an eagerly executed declaration.
    getServerSnapshot: getSnapshot,
    subscribe(listener: () => void): () => void {
      listeners.add(listener);
      if (listeners.size === 1) {
        stopScope = metadata.scope.subscribe(attach);
        // A StrictMode cleanup/reconnect needs a new watch, even in the same epoch.
        runtime = undefined;
        attach();
      }
      return () => {
        listeners.delete(listener);
        if (!listeners.size) { disconnect(); stopScope?.(); stopScope = undefined; }
      };
    },
  };
}

/** Constructing an observer is pure: subscribe starts the live watch, never call(). */
export function createQueryObserver<Q extends QueryFunction>(query: Q, ...args: QueryArguments<Q>) {
  const { input, options } = queryArguments(query, args);
  const metadata = bindingInfo(query);
  return createObservation<Output<Q>>(query, options, (owner, receive) => {
    if (!owner.watch) throw new Error("The configured runtime does not support live queries");
    return owner.watch({ name: metadata.name, kind: "query", input,
      ...(metadata.codecs ? { codecs: metadata.codecs } : {}) },
    stagedObserver(raw => receive(querySnapshot<Output<Q>>(raw, metadata.codecs))), {});
  });
}

/** Observe the core page handle; page ranges and rows stay owned by that handle. */
export function createPaginationObserver<Q extends PaginatedQueryFunction>(query: Q,
  mapper: (cursor: string | null) => Input<Q>, options: PaginationBindingOptions<NoInfer<PageRow<Q>>> = {}) {
  if (typeof query.paginate !== "function") throw new TypeError("usePaginatedQuery requires a paginated query");
  if (typeof mapper !== "function") throw new TypeError("Pagination requires an input mapper");
  let handle: PageHandle<PageRow<Q>> | undefined;
  const next = (pages = 1): Promise<void> => handle
    ? handle.next(pages)
    : Promise.reject(new Error("Mount the paginated query before loading more pages"));
  const observation = createObservation<readonly PageRow<Q>[]>(query, options, (_owner, receive) => {
    const active = query.paginate(mapper as (cursor: string | null) => never) as PageHandle<PageRow<Q>>;
    handle = active;
    const stop = active.subscribe(receive);
    return () => { if (handle === active) handle = undefined; stop(); active.dispose(); };
  });
  let previous: QueryState<readonly PageRow<Q>[]> | undefined;
  let snapshot: PaginationSnapshot<PageRow<Q>>;
  const getSnapshot = (): PaginationSnapshot<PageRow<Q>> => {
    const state = observation.getSnapshot();
    const hasMore = handle?.hasMore ?? false;
    if (previous !== state || snapshot.hasMore !== hasMore) {
      previous = state;
      snapshot = { ...state, hasMore, next };
    }
    return snapshot;
  };
  return { ...observation, getSnapshot, getServerSnapshot: getSnapshot };
}

export function createMutationObserver<M extends MutationFunction>(mutation: M) {
  if (bindingInfo(mutation).kind !== "mutation") throw new TypeError("useMutation requires a mutation");
  const idle: MutationBindingState<Output<M>> = { status: "idle" };
  let state: MutationBindingState<Output<M>> = idle;
  let handle: MutationHandle<Output<M>> | undefined;
  let stop: (() => void) | undefined;
  let invocation = 0;
  const listeners = new Set<() => void>();
  const observe = () => {
    stop?.(); stop = undefined;
    if (!handle) return;
    const id = invocation;
    stop = handle.subscribe(next => {
      if (id !== invocation) return;
      state = next;
      emit(listeners);
    });
  };
  return {
    getSnapshot: (): MutationBindingState<Output<M>> => {
      if (handle && handle.status !== state.status) {
        const status = handle.status;
        state = status === "success" ? { status, data: handle.data! }
          : status === "error" ? { status, error: handle.error } : { status };
      }
      return state;
    },
    getServerSnapshot: (): MutationBindingState<Output<M>> => idle,
    subscribe(listener: () => void): () => void {
      listeners.add(listener);
      if (listeners.size === 1) observe();
      return () => { listeners.delete(listener); if (!listeners.size) { stop?.(); stop = undefined; } };
    },
    invoke(...args: MutationArguments<M>): MutationHandle<Output<M>> {
      invocation++;
      handle = Reflect.apply(mutation, undefined, args) as MutationHandle<Output<M>>;
      state = { status: "pending" };
      if (listeners.size) observe();
      return handle;
    },
  };
}
