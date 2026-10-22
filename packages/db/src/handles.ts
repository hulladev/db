import { derivedObserver, notification, notifyObserver } from "./notifications.ts";
import { DatabaseError, isApplicationError } from "./error.ts";
import type {
  FunctionCodecs,
} from "./codec.ts";
import type {
  MutationHandle, MutationState, QueryHandle, QueryState, QuerySubscriptionOptions,
  ReadonlyResult,
} from "./contracts.ts";

function notify<T>(listener: (state: T) => void, state: T): void {
  notifyObserver(listener, () => { try { listener(state); } catch { /* Listener failures belong to the application. */ } });
}

/** Observed handles follow auth retirement without retaining unobserved completed operations. */
function observeScope(signal: AbortSignal | undefined, checkScope: () => void): () => void {
  if (!signal) return () => {};
  if (signal.aborted) { checkScope(); return () => {}; }
  signal.addEventListener("abort", checkScope, { once: true });
  return () => { signal.removeEventListener("abort", checkScope); };
}

function assertScopeSignal(signal: AbortSignal | undefined): void {
  if (signal?.aborted) throw new DatabaseError({ code: "context_changed", message: "Operation authorization scope changed" });
}

/** Check ordinary shared values in development. Date methods are intentionally unaffected. */
function readonly<T>(value: T, seen = new WeakSet<object>()): ReadonlyResult<T> {
  if (typeof process !== "undefined" && process.env.NODE_ENV === "production") return value as ReadonlyResult<T>;
  if (value !== null && typeof value === "object" && !seen.has(value)
    && (Array.isArray(value) || [Object.prototype, null].includes(Object.getPrototypeOf(value)))) {
    seen.add(value);
    for (const item of Object.values(value)) readonly(item, seen);
    Object.freeze(value);
  }
  return value as ReadonlyResult<T>;
}

export function querySnapshot<T>(state: QueryState<unknown>, codecs?: FunctionCodecs): QueryState<T> {
  try {
    if (state.status === "success" || ("data" in state && state.data !== undefined)) {
      const data = readonly(codecs ? codecs.result.parse(state.data) : state.data) as ReadonlyResult<T>;
      return { ...state, data } as QueryState<T>;
    }
    return state as QueryState<T>;
  } catch (error) { return { status: "error", error }; }
}

export function subscriptionOptions(value: unknown): QuerySubscriptionOptions {
  if (value === undefined) return {};
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new TypeError("Subscription options must be an object");
  if (Object.keys(value).some(key => key !== "signal" && key !== "keepPreviousDataOnError")) throw new TypeError("Unknown subscription option");
  const options = value as QuerySubscriptionOptions;
  if (options.signal !== undefined && !(options.signal instanceof AbortSignal)) throw new TypeError("signal must be an AbortSignal");
  if (options.keepPreviousDataOnError !== undefined && typeof options.keepPreviousDataOnError !== "boolean") throw new TypeError("keepPreviousDataOnError must be a boolean");
  return options;
}

export function clearsPrevious(error: unknown): boolean {
  if (typeof error !== "object" || error === null || !("code" in error)) return false;
  return !isApplicationError(error) && ["unauthorized", "forbidden", "reset_required", "context_changed", "disposed", "authentication", "authorization", "authentication_failed"]
    .includes(String(error.code));
}

/** Retention belongs to each subscriber; the authoritative handle/cache still receives the error. */
export function queryListener<T>(listener: (state: QueryState<T>) => void, options: QuerySubscriptionOptions): (state: QueryState<T>) => void {
  let previous: ReadonlyResult<T> | undefined;
  let hasPrevious = false;
  return derivedObserver(listener, (state: QueryState<T>) => {
    if (state.status === "success") { previous = state.data; hasPrevious = true; }
    if (state.status === "error" && clearsPrevious(state.error)) { previous = undefined; hasPrevious = false; }
    if (state.status === "error" && options.keepPreviousDataOnError && hasPrevious) {
      notify(listener, { ...state, data: previous!, isStale: true, isOptimistic: false });
    } else notify(listener, state);
  });
}

export class QueryOperation<T> implements QueryHandle<T> {
  #state: QueryState<T> = { status: "pending" };
  #outcome: Promise<ReadonlyResult<T>>;
  #listeners = new Set<(state: QueryState<T>) => void>();
  #subscriptions = new Set<() => void>();
  #watch: ((listener: (state: QueryState<T>) => void) => () => void) | undefined;
  #stop: (() => void) | undefined;
  #observed = false;
  #assertScope: (() => void) | undefined;
  #scopeSignal: (() => AbortSignal | undefined) | undefined;
  #stopScope: (() => void) | undefined;
  #retired = false;
  #scopeError: unknown;
  #signal: (() => AbortSignal | undefined) | undefined;
  #cancelled = false;

  constructor(start: () => Promise<unknown>, watch?: (listener: (state: QueryState<T>) => void) => () => void, codecs?: FunctionCodecs, assertScope?: () => void, signal?: () => AbortSignal | undefined, scopeSignal?: () => AbortSignal | undefined) {
    this.#watch = watch;
    this.#assertScope = assertScope;
    this.#scopeSignal = scopeSignal;
    this.#signal = signal;
    let execution: Promise<unknown>;
    try { execution = Promise.resolve(start()); } catch (error) { execution = Promise.reject(error); }
    this.#outcome = execution.then(
      (result) => {
        this.#checkScope();
        if (this.#retired) throw this.#scopeError;
        this.#signal?.()?.throwIfAborted();
        const data = readonly(codecs ? codecs.result.parse(result) : result) as ReadonlyResult<T>;
        if (!this.#observed) this.#publish({ status: "success", data, isStale: false, isOptimistic: false });
        this.#checkScope();
        if (this.#retired) throw this.#scopeError;
        return data;
      },
    ).catch((error: unknown) => {
      this.#checkScope();
      if (this.#retired) throw this.#scopeError;
      const signal = this.#signal?.();
      if (signal?.aborted) {
        this.#cancelled = true;
        const stop = this.#stop;
        this.#stop = undefined;
        stop?.();
        error = signal.reason ?? error;
        this.#publish({ status: "error", error });
        for (const unsubscribe of [...this.#subscriptions]) unsubscribe();
      } else if (!this.#observed) this.#publish({ status: "error", error });
      throw error;
    });
    // Non-awaited operations are supported: errors remain inspectable and later awaits still reject.
    void this.#outcome.catch(() => {});
  }

  #checkScope(): void {
    if (this.#retired) return;
    try { this.#assertScope?.(); assertScopeSignal(this.#scopeSignal?.()); }
    catch (error) {
      this.#retired = true;
      this.#scopeError = error;
      this.#stopScope?.();
      this.#stopScope = undefined;
      const stop = this.#stop;
      this.#stop = undefined;
      stop?.();
      this.#publish({ status: "error", error });
      for (const unsubscribe of [...this.#subscriptions]) unsubscribe();
    }
  }
  get status() { this.#checkScope(); return this.#state.status; }
  get data(): ReadonlyResult<T> | undefined { this.#checkScope(); return "data" in this.#state ? this.#state.data : undefined; }
  get error(): unknown { this.#checkScope(); return this.#state.status === "error" ? this.#state.error : undefined; }
  get isStale(): boolean { this.#checkScope(); return this.#state.isStale ?? false; }
  get isOptimistic(): boolean { this.#checkScope(); return this.#state.isOptimistic ?? false; }
  then<A = ReadonlyResult<T>, B = never>(fulfilled?: ((value: ReadonlyResult<T>) => A | PromiseLike<A>) | null, rejected?: ((reason: unknown) => B | PromiseLike<B>) | null): Promise<A | B> {
    this.#checkScope();
    const outcome = this.#retired ? Promise.reject<ReadonlyResult<T>>(this.#scopeError) : this.#outcome.then(value => {
      this.#checkScope();
      if (this.#retired) throw this.#scopeError;
      return value;
    });
    return outcome.then(fulfilled, rejected);
  }

  #publish(state: QueryState<T>): void {
    this.#checkScope();
    if (this.#retired && !(state.status === "error" && state.error === this.#scopeError)) return;
    this.#state = state;
    for (const listener of [...this.#listeners]) {
      this.#checkScope();
      if (this.#state !== state) break;
      notification(() => { if (this.#state === state && this.#listeners.has(listener)) notify(listener, state); });
    }
    this.#checkScope();
  }

  subscribe(listener: (state: QueryState<T>) => void, options: QuerySubscriptionOptions = {}): () => void {
    if (typeof listener !== "function") throw new TypeError("A query subscription needs a listener");
    options = subscriptionOptions(options);
    options.signal?.throwIfAborted();
    this.#checkScope();
    if (this.#retired || this.#cancelled) { notify(listener, this.#state); return () => {}; }
    if (!this.#watch) throw new Error("The configured runtime does not support query subscriptions");
    const deliver = queryListener(listener, options);
    const first = this.#listeners.size === 0;
    this.#listeners.add(deliver);
    let active = true;
    const stop = () => {
      if (!active) return;
      active = false;
      options.signal?.removeEventListener("abort", stop);
      this.#listeners.delete(deliver);
      this.#subscriptions.delete(stop);
      if (!this.#listeners.size) {
        this.#stopScope?.();
        this.#stopScope = undefined;
        const stopWatch = this.#stop;
        this.#stop = undefined;
        stopWatch?.();
      }
    };
    this.#subscriptions.add(stop);
    options.signal?.addEventListener("abort", stop, { once: true });
    if (first) this.#stopScope = observeScope(this.#scopeSignal?.(), () => this.#checkScope());
    if (this.#retired) { stop(); return stop; }
    deliver(this.#state);
    this.#checkScope();
    if (this.#retired) { stop(); return stop; }
    if (first && active) {
      const previouslyObserved = this.#observed;
      // Once attached, live delivery owns handle state even before its first
      // round. The independent one-shot still resolves the awaitable outcome.
      this.#observed = true;
      try {
        const stopWatch = this.#watch((state) => {
          this.#checkScope();
          if (this.#retired || this.#cancelled) return;
          if (state.status !== "pending") this.#observed = true;
          this.#publish(state);
        });
        if (!this.#listeners.size) stopWatch();
        else this.#stop = stopWatch;
      } catch (error) { this.#observed = previouslyObserved; stop(); throw error; }
    }
    return stop;
  }
}

export class MutationOperation<T> implements MutationHandle<T> {
  #state: MutationState<T> = { status: "pending" };
  #outcome: Promise<T>;
  #listeners = new Set<(state: MutationState<T>) => void>();
  #assertScope: (() => void) | undefined;
  #scopeSignal: (() => AbortSignal | undefined) | undefined;
  #stopScope: (() => void) | undefined;
  #retired = false;
  #scopeError: unknown;

  constructor(start: (onStatus: (status: "recovering") => void) => Promise<unknown>, codecs?: FunctionCodecs, assertScope?: () => void, scopeSignal?: () => AbortSignal | undefined) {
    this.#assertScope = assertScope;
    this.#scopeSignal = scopeSignal;
    let execution: Promise<unknown>;
    try {
      execution = Promise.resolve(start(() => {
        this.#checkScope();
        if (this.#state.status === "pending") this.#publish({ status: "recovering" });
      }));
    } catch (error) { execution = Promise.reject(error); }
    this.#outcome = execution.then((result) => {
      this.#checkScope();
      if (this.#retired) throw this.#scopeError;
      const data = (codecs ? codecs.result.parse(result) : result) as T;
      this.#publish({ status: "success", data });
      this.#checkScope();
      if (this.#retired) throw this.#scopeError;
      return data;
    }).catch((error: unknown) => {
      this.#checkScope();
      if (this.#retired) throw this.#scopeError;
      this.#publish({ status: "error", error });
      if (this.#retired) throw this.#scopeError;
      throw error;
    });
    void this.#outcome.catch(() => {});
  }

  #checkScope(): void {
    if (this.#retired) return;
    try { this.#assertScope?.(); assertScopeSignal(this.#scopeSignal?.()); }
    catch (error) {
      this.#retired = true;
      this.#scopeError = error;
      this.#stopScope?.();
      this.#stopScope = undefined;
      this.#publish({ status: "error", error });
      this.#listeners.clear();
    }
  }
  get status() { this.#checkScope(); return this.#state.status; }
  get data(): T | undefined { this.#checkScope(); return this.#state.status === "success" ? this.#state.data : undefined; }
  get error(): unknown { this.#checkScope(); return this.#state.status === "error" ? this.#state.error : undefined; }
  then<A = T, B = never>(fulfilled?: ((value: T) => A | PromiseLike<A>) | null, rejected?: ((reason: unknown) => B | PromiseLike<B>) | null): Promise<A | B> {
    this.#checkScope();
    const outcome = this.#retired ? Promise.reject<T>(this.#scopeError) : this.#outcome.then(value => {
      this.#checkScope();
      if (this.#retired) throw this.#scopeError;
      return value;
    });
    return outcome.then(fulfilled, rejected);
  }
  #publish(state: MutationState<T>): void {
    this.#checkScope();
    if (this.#retired && !(state.status === "error" && state.error === this.#scopeError)) return;
    this.#state = state;
    for (const listener of [...this.#listeners]) {
      this.#checkScope();
      if (this.#state !== state) break;
      notification(() => { if (this.#state === state && this.#listeners.has(listener)) notify(listener, state); });
    }
    this.#checkScope();
  }
  subscribe(listener: (state: MutationState<T>) => void): () => void {
    if (typeof listener !== "function") throw new TypeError("A mutation subscription needs a listener");
    this.#checkScope();
    if (this.#retired) { notify(listener, this.#state); return () => {}; }
    const deliver = (state: MutationState<T>) => notify(listener, state);
    const first = this.#listeners.size === 0;
    this.#listeners.add(deliver);
    const stop = () => {
      this.#listeners.delete(deliver);
      if (!this.#listeners.size) {
        this.#stopScope?.();
        this.#stopScope = undefined;
      }
    };
    if (first) this.#stopScope = observeScope(this.#scopeSignal?.(), () => this.#checkScope());
    if (this.#retired) { stop(); return stop; }
    deliver(this.#state);
    this.#checkScope();
    return stop;
  }
}
