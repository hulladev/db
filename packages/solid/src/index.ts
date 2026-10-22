import { createEffect, createSignal, onCleanup, onMount, untrack, type Accessor } from "solid-js";
import {
  createMutationObserver, createPaginationObserver, createQueryObserver, queryArguments, queryKey,
  type MutationArguments, type MutationBindingState, type MutationFunction,
  type PaginatedQueryFunction, type PageRow, type PaginationBindingOptions, type PaginationSnapshot,
  type QueryArguments, type QueryBindingOptions, type QueryFunction,
} from "@hulla/db/bindings";
import type { Input, MutationHandle, Output, QueryState } from "@hulla/db";

export type ReactiveQueryArguments<Q extends QueryFunction> = [Input<Q>] extends [undefined]
  ? QueryArguments<Q>
  : [input: Input<Q> | Accessor<Input<Q>>, options?: QueryBindingOptions<NoInfer<Output<Q>>>];

/** Call in a component owner. Live observation starts after mount. */
export function createQuery<Q extends QueryFunction>(query: Q, ...args: ReactiveQueryArguments<Q>): Accessor<QueryState<Output<Q>>> {
  const { input: source, options } = queryArguments(query, args as QueryArguments<Q>);
  const readInput = (): Input<Q> => typeof source === "function" ? (source as Accessor<Input<Q>>)() : source;
  const { initialData: _initialData, initialAuthKey: _initialAuthKey, ...liveOptions } = options;
  const observe = (input: Input<Q>, seed: boolean) => {
    const resolved: unknown[] = [...args];
    if (source !== undefined) { resolved[0] = input; resolved[1] = seed ? options : liveOptions; }
    return createQueryObserver(query, ...resolved as QueryArguments<Q>);
  };
  let key = queryKey(query, readInput());
  let observer = observe(readInput(), true);
  const [state, setState] = createSignal(observer.getServerSnapshot());
  let stop: (() => void) | undefined;
  onMount(() => {
    createEffect(() => {
      const input = readInput();
      const nextKey = queryKey(query, input);
      untrack(() => {
        if (nextKey !== key) {
          stop?.(); key = nextKey;
          observer = observe(input, false);
        } else if (stop) return;
        setState(() => observer.getSnapshot());
        stop = observer.subscribe(() => setState(() => observer.getSnapshot()));
        setState(() => observer.getSnapshot());
      });
    });
  });
  onCleanup(() => stop?.());
  return state;
}

export type PaginatedQueryResult<Q extends PaginatedQueryFunction> = Accessor<PaginationSnapshot<PageRow<Q>>> & {
  readonly next: PaginationSnapshot<PageRow<Q>>["next"];
};

/** Call in a component owner. The mapper reads reactive filters and supplies the page cursor. */
export function createPaginatedQuery<Q extends PaginatedQueryFunction>(
  query: Q,
  mapper: (cursor: string | null) => Input<Q>,
  options: PaginationBindingOptions<NoInfer<PageRow<Q>>> = {},
): PaginatedQueryResult<Q> {
  const { initialData: _initialData, initialAuthKey: _initialAuthKey, ...liveOptions } = options;
  let key = queryKey(query, mapper(null));
  let observer = createPaginationObserver(query, mapper, options);
  const [state, setState] = createSignal(observer.getServerSnapshot());
  let stop: (() => void) | undefined;
  onMount(() => {
    createEffect(() => {
      const nextKey = queryKey(query, mapper(null));
      untrack(() => {
        if (nextKey !== key) {
          stop?.(); key = nextKey;
          observer = createPaginationObserver(query, mapper, liveOptions);
        } else if (stop) return;
        setState(() => observer.getSnapshot());
        stop = observer.subscribe(() => setState(() => observer.getSnapshot()));
        setState(() => observer.getSnapshot());
      });
    });
  });
  onCleanup(() => stop?.());
  return Object.assign(state, { next: (pages?: number) => observer.getSnapshot().next(pages) });
}

export type MutationResult<M extends MutationFunction> = ((...args: MutationArguments<M>) => MutationHandle<Output<M>>) & {
  readonly state: Accessor<MutationBindingState<Output<M>>>;
};

export function createMutation<M extends MutationFunction>(mutation: M): MutationResult<M> {
  const observer = createMutationObserver(mutation);
  const [state, setState] = createSignal(observer.getServerSnapshot());
  let stop: (() => void) | undefined;
  onMount(() => {
    stop = observer.subscribe(() => setState(() => observer.getSnapshot()));
    setState(() => observer.getSnapshot());
  });
  onCleanup(() => stop?.());
  return Object.assign((...args: MutationArguments<M>) => observer.invoke(...args), { state });
}
