"use client";

import { useCallback, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import {
  createMutationObserver,
  createQueryObserver,
  createPaginationObserver,
  type PaginatedQueryFunction,
  type PageRow,
  type PaginationSnapshot,
  type PaginationBindingOptions,
  queryArguments,
  queryKey,
  type MutationArguments,
  type MutationFunction,
  type QueryArguments,
  type QueryFunction,
} from "@hulla/db/bindings";
import type { Input, MutationHandle, MutationState, Output, QueryState } from "@hulla/db";

/** Observe a generated query. Network observation starts when React subscribes. */
export function useQuery<Q extends QueryFunction>(query: Q, ...args: QueryArguments<Q>): QueryState<Output<Q>> {
  const { input, options, hasInput } = queryArguments(query, args);
  const key = queryKey(query, input);
  const [seed, setSeed] = useState(() => ({ query, key, options, valid: true }));
  const eligible = seed.valid && seed.query === query && seed.key === key
    && seed.options.initialAuthKey === options.initialAuthKey;
  if (seed.valid && !eligible) setSeed({ ...seed, valid: false });
  const observer = useMemo(() => {
    const settings = {
      ...(options.keepPreviousDataOnError === undefined ? {} : { keepPreviousDataOnError: options.keepPreviousDataOnError }),
      ...(eligible && Object.hasOwn(seed.options, "initialData") ? { initialData: seed.options.initialData } : {}),
      ...(eligible && seed.options.initialAuthKey !== undefined ? { initialAuthKey: seed.options.initialAuthKey } : {}),
    };
    return createQueryObserver(query, ...(hasInput ? [input, settings] : [settings]) as QueryArguments<Q>);
  }, [query, key, eligible, options.keepPreviousDataOnError]);
  return useSyncExternalStore(observer.subscribe, observer.getSnapshot, observer.getServerSnapshot);
}

/** Observe a growing list of live page ranges from a direct paginated query. */
export function usePaginatedQuery<Q extends PaginatedQueryFunction>(query: Q,
  mapper: (cursor: string | null) => Input<Q>, options: PaginationBindingOptions<NoInfer<PageRow<Q>>> = {}): PaginationSnapshot<PageRow<Q>> {
  const key = queryKey(query, mapper(null));
  const [seed, setSeed] = useState(() => ({ query, key, options, valid: true }));
  const eligible = seed.valid && seed.query === query && seed.key === key
    && seed.options.initialAuthKey === options.initialAuthKey;
  if (seed.valid && !eligible) setSeed({ ...seed, valid: false });
  const observer = useMemo(() => createPaginationObserver(query, mapper, {
    ...(options.keepPreviousDataOnError === undefined ? {} : { keepPreviousDataOnError: options.keepPreviousDataOnError }),
    ...(eligible && Object.hasOwn(seed.options, "initialData") ? { initialData: seed.options.initialData } : {}),
    ...(eligible && seed.options.initialAuthKey !== undefined ? { initialAuthKey: seed.options.initialAuthKey } : {}),
  }), [query, key, eligible, options.keepPreviousDataOnError]);
  const active = useRef(observer);
  useLayoutEffect(() => { active.current = observer; }, [observer]);
  const next = useCallback((pages = 1) => active.current.getSnapshot().next(pages), []);
  const snapshot = useSyncExternalStore(observer.subscribe, observer.getSnapshot, observer.getServerSnapshot);
  return useMemo(() => ({ ...snapshot, next }), [snapshot, next]);
}

/** A render's mutation snapshot accompanies the callable that starts writes. */
export type MutationResult<M extends MutationFunction> = ((...args: MutationArguments<M>) => MutationHandle<Output<M>>) & {
  readonly state: MutationState<Output<M>> | { readonly status: "idle" };
};

export function useMutation<M extends MutationFunction>(mutation: M): MutationResult<M> {
  const observer = useMemo(() => createMutationObserver(mutation), [mutation]);
  const state = useSyncExternalStore(observer.subscribe, observer.getSnapshot, observer.getServerSnapshot);
  return useMemo(() => Object.assign(
    (...args: MutationArguments<M>) => observer.invoke(...args),
    { state },
  ), [observer, state]);
}
