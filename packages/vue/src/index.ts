import { onMounted, onScopeDispose, shallowReadonly, shallowRef, watch, type ShallowRef } from "vue";
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

/** Call during setup. Live observation starts after the component mounts. */
export function useQuery<Q extends QueryFunction>(query: Q, ...args: ReactiveQueryArguments<Q>): Readonly<ShallowRef<QueryState<Output<Q>>>> {
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
  const state = shallowRef(observer.getServerSnapshot());
  let stop: (() => void) | undefined;
  let stopInputs: (() => void) | undefined;
  onMounted(() => {
    stopInputs = watch(() => queryKey(query, readInput()), nextKey => {
      if (nextKey !== key) {
        stop?.(); key = nextKey;
        observer = observe(readInput(), false);
      }
      state.value = observer.getSnapshot();
      stop = observer.subscribe(() => { state.value = observer.getSnapshot(); });
      state.value = observer.getSnapshot();
    }, { immediate: true, flush: "sync" });
  });
  onScopeDispose(() => { stopInputs?.(); stop?.(); });
  return shallowReadonly(state);
}

export type PaginatedQueryResult<Q extends PaginatedQueryFunction> = Readonly<ShallowRef<PaginationSnapshot<PageRow<Q>>>> & {
  readonly next: PaginationSnapshot<PageRow<Q>>["next"];
};

/** Call during setup. The mapper reads reactive filters and supplies the page cursor. */
export function usePaginatedQuery<Q extends PaginatedQueryFunction>(
  query: Q,
  mapper: (cursor: string | null) => Input<Q>,
  options: PaginationBindingOptions<NoInfer<PageRow<Q>>> = {},
): PaginatedQueryResult<Q> {
  const { initialData: _initialData, initialAuthKey: _initialAuthKey, ...liveOptions } = options;
  let key = queryKey(query, mapper(null));
  let observer = createPaginationObserver(query, mapper, options);
  const state = shallowRef(observer.getServerSnapshot());
  let stop: (() => void) | undefined;
  let stopInputs: (() => void) | undefined;
  onMounted(() => {
    stopInputs = watch(() => queryKey(query, mapper(null)), nextKey => {
      if (nextKey !== key) {
        stop?.(); key = nextKey;
        observer = createPaginationObserver(query, mapper, liveOptions);
      }
      state.value = observer.getSnapshot();
      stop = observer.subscribe(() => { state.value = observer.getSnapshot(); });
      state.value = observer.getSnapshot();
    }, { immediate: true, flush: "sync" });
  });
  onScopeDispose(() => { stopInputs?.(); stop?.(); });
  return shallowReadonly(Object.assign(state, { next: (pages?: number) => observer.getSnapshot().next(pages) }));
}

export type MutationResult<M extends MutationFunction> = ((...args: MutationArguments<M>) => MutationHandle<Output<M>>) & {
  readonly state: Readonly<ShallowRef<MutationBindingState<Output<M>>>>;
};

export function useMutation<M extends MutationFunction>(mutation: M): MutationResult<M> {
  const observer = createMutationObserver(mutation);
  const state = shallowRef(observer.getServerSnapshot());
  let stop: (() => void) | undefined;
  onMounted(() => {
    stop = observer.subscribe(() => { state.value = observer.getSnapshot(); });
    state.value = observer.getSnapshot();
  });
  onScopeDispose(() => stop?.());
  return Object.assign((...args: MutationArguments<M>) => observer.invoke(...args), { state: shallowReadonly(state) });
}
