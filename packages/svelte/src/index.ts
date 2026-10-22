import { onMount } from "svelte";
import { createSubscriber } from "svelte/reactivity";
import { toStore } from "svelte/store";
import {
  createMutationObserver, createPaginationObserver, createQueryObserver, queryArguments, queryKey,
  type MutationArguments, type MutationBindingState, type MutationFunction,
  type PaginatedQueryFunction, type PageRow, type PaginationBindingOptions, type PaginationSnapshot,
  type QueryArguments, type QueryBindingOptions, type QueryFunction,
} from "@hulla/db/bindings";
import type { Input, MutationHandle, Output, QueryState } from "@hulla/db";

export type ReactiveQueryArguments<Q extends QueryFunction> = [Input<Q>] extends [undefined]
  ? QueryArguments<Q>
  : [input: Input<Q> | (() => Input<Q>), options?: QueryBindingOptions<NoInfer<Output<Q>>>];

/** Call during component initialization. Read properties reactively without destructuring. */
export function createQuery<Q extends QueryFunction>(query: Q, ...args: ReactiveQueryArguments<Q>): QueryState<Output<Q>> {
  const { input: source, options } = queryArguments(query, args as QueryArguments<Q>);
  const readInput = (): Input<Q> => typeof source === "function" ? (source as () => Input<Q>)() : source;
  const { initialData: _initialData, initialAuthKey: _initialAuthKey, ...liveOptions } = options;
  const observe = (input: Input<Q>, seed: boolean) => {
    const resolved: unknown[] = [...args];
    if (source !== undefined) { resolved[0] = input; resolved[1] = seed ? options : liveOptions; }
    return createQueryObserver(query, ...resolved as QueryArguments<Q>);
  };
  let key = queryKey(query, readInput());
  let observer = observe(readInput(), true);
  const updates = new Set<() => void>();
  const track = createSubscriber(update => { updates.add(update); return () => { updates.delete(update); }; });
  const notify = () => { for (const update of updates) update(); };
  // toStore tracks the getter's rune dependencies using Svelte's public API;
  // subscribing only after mount keeps SSR and discarded initialization pure.
  const inputs = toStore(() => { const input = readInput(); return { input, key: queryKey(query, input) }; });
  onMount(() => {
    let stop: (() => void) | undefined;
    const stopInputs = inputs.subscribe(next => {
      if (next.key !== key) {
        stop?.(); key = next.key;
        observer = observe(next.input, false);
      } else if (stop) return;
      stop = observer.subscribe(notify);
      notify();
    });
    return () => { stopInputs(); stop?.(); };
  });
  const snapshot = () => { track(); return observer.getSnapshot(); };
  return {
    get status() { return snapshot().status; },
    get data() { return snapshot().data; },
    get error() { const state = snapshot(); return state.status === "error" ? state.error : undefined; },
    get isStale() { return snapshot().isStale; },
    get isOptimistic() { return snapshot().isOptimistic; },
  } as QueryState<Output<Q>>;
}

/** Call during initialization. Read properties reactively without destructuring. */
export function createPaginatedQuery<Q extends PaginatedQueryFunction>(
  query: Q,
  mapper: (cursor: string | null) => Input<Q>,
  options: PaginationBindingOptions<NoInfer<PageRow<Q>>> = {},
): PaginationSnapshot<PageRow<Q>> {
  const { initialData: _initialData, initialAuthKey: _initialAuthKey, ...liveOptions } = options;
  let key = queryKey(query, mapper(null));
  let observer = createPaginationObserver(query, mapper, options);
  const updates = new Set<() => void>();
  const track = createSubscriber(update => { updates.add(update); return () => { updates.delete(update); }; });
  const notify = () => { for (const update of updates) update(); };
  const inputs = toStore(() => queryKey(query, mapper(null)));
  onMount(() => {
    let stop: (() => void) | undefined;
    const stopInputs = inputs.subscribe(nextKey => {
      if (nextKey !== key) {
        stop?.(); key = nextKey;
        observer = createPaginationObserver(query, mapper, liveOptions);
      } else if (stop) return;
      stop = observer.subscribe(notify);
      notify();
    });
    return () => { stopInputs(); stop?.(); };
  });
  const snapshot = () => { track(); return observer.getSnapshot(); };
  return {
    get status() { return snapshot().status; },
    get data() { return snapshot().data; },
    get error() { const state = snapshot(); return state.status === "error" ? state.error : undefined; },
    get isStale() { return snapshot().isStale; },
    get isOptimistic() { return snapshot().isOptimistic; },
    get hasMore() { return snapshot().hasMore; },
    next: (pages?: number) => observer.getSnapshot().next(pages),
  } as PaginationSnapshot<PageRow<Q>>;
}

export type MutationResult<M extends MutationFunction> = ((...args: MutationArguments<M>) => MutationHandle<Output<M>>) & {
  readonly state: MutationBindingState<Output<M>>;
};

export function createMutation<M extends MutationFunction>(mutation: M): MutationResult<M> {
  const observer = createMutationObserver(mutation);
  const updates = new Set<() => void>();
  const track = createSubscriber(update => { updates.add(update); return () => { updates.delete(update); }; });
  onMount(() => observer.subscribe(() => { for (const update of updates) update(); }));
  return Object.defineProperty((...args: MutationArguments<M>) => observer.invoke(...args), "state", {
    enumerable: true,
    get() { track(); return observer.getSnapshot(); },
  }) as MutationResult<M>;
}
