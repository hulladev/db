import type { FunctionCodecs } from "./codec.ts";
import type { FunctionRuntime, Operation, PageHandle, QueryState, QuerySubscriptionOptions, ReadonlyResult } from "./contracts.ts";
import type { Page } from "./page.ts";
import { DatabaseError, isApplicationError } from "./error.ts";
import { queryListener, querySnapshot, subscriptionOptions } from "./handles.ts";
import { afterBatch, notification, notifyObserver, stagedObserver } from "./notifications.ts";
import { canonical } from "./http/protocol.ts";

type Slot<T> = {
  operation: Operation & { kind: "query" };
  inputKey: string;
  state: QueryState<Page<T>>;
  stop?: () => void;
};

function protectedFailure(error: unknown): boolean {
  return typeof error === "object" && error !== null && "code" in error
    && !isApplicationError(error) && ["unauthorized", "forbidden", "reset_required", "context_changed", "disposed", "authentication", "authorization"].includes(String(error.code));
}

/** A list owns its ranges and publishes only complete adapter rounds. */
export class PaginationOperation<I, T> implements PageHandle<T> {
  #state: QueryState<readonly T[]> = { status: "pending" };
  #slots: Slot<T>[] = [];
  #listeners = new Set<(state: QueryState<readonly T[]>) => void>();
  #runtime: FunctionRuntime;
  #mapper: (cursor: string | null) => I;
  #baseline: string;
  #name: string;
  #codecs: FunctionCodecs | undefined;
  #owner = crypto.randomUUID();
  #disposed = false;
  #queued = false;
  #loading: Promise<void> | undefined;
  #appending: Slot<T> | undefined;
  #appendPrevious: ReadonlyResult<readonly T[]> | undefined;
  #first: Promise<ReadonlyResult<readonly T[]>>;
  #waiters = new Set<() => void>();
  #identitySignal: AbortSignal | undefined;
  #identityEpoch: unknown;
  #abort = () => this.#retire(new DatabaseError({ code: "context_changed", message: "Pagination authorization scope changed" }));
  constructor(runtime: FunctionRuntime, name: string, mapper: (cursor: string | null) => I, codecs?: FunctionCodecs) {
    if (typeof mapper !== "function") throw new TypeError("Pagination requires an input mapper");
    if (!runtime.watch) throw new Error("The configured runtime does not support query subscriptions");
    this.#runtime = runtime;
    this.#name = name;
    this.#mapper = mapper;
    this.#codecs = codecs;
    this.#baseline = this.#encode(mapper(null));
    this.#identityEpoch = runtime.identityEpoch;
    this.#identitySignal = runtime.identitySignal;
    this.#identitySignal?.addEventListener("abort", this.#abort, { once: true });
    this.#checkIdentity();
    this.#first = this.next().then(() => {
      this.#checkIdentity();
      if (this.#state.status === "error") throw this.#state.error;
      return this.data!;
    });
    void this.#first.catch(() => {});
  }
  #encode(input: I): string { return canonical(this.#codecs ? this.#codecs.input.encode(input) : input); }
  #checkIdentity(): void {
    if (!this.#disposed && (this.#runtime.identityEpoch !== this.#identityEpoch || this.#identitySignal?.aborted)) this.#abort();
  }
  get status() { this.#checkIdentity(); return this.#state.status; }
  get data() { this.#checkIdentity(); return "data" in this.#state ? this.#state.data : undefined; }
  get error() { this.#checkIdentity(); return this.#state.status === "error" ? this.#state.error : undefined; }
  get isStale() { this.#checkIdentity(); return this.#state.isStale ?? false; }
  get isOptimistic() { this.#checkIdentity(); return this.#state.isOptimistic ?? false; }
  get hasMore() { this.#checkIdentity(); const last = this.#slots.at(-1)?.state; return last?.status === "success" && last.data.cursor !== null; }
  then<A = ReadonlyResult<readonly T[]>, B = never>(fulfilled?: ((value: ReadonlyResult<readonly T[]>) => A | PromiseLike<A>) | null, rejected?: ((reason: unknown) => B | PromiseLike<B>) | null): Promise<A | B> { return this.#first.then(value => {
    this.#checkIdentity();
    if (this.#disposed) throw this.error;
    return value;
  }).then(fulfilled, rejected); }
  #publish(state: QueryState<readonly T[]>): void {
    this.#state = state;
    for (const listener of [...this.#listeners]) {
      this.#checkIdentity();
      if (this.#state !== state) break;
      this.#deliver(listener, state);
    }
    for (const wake of [...this.#waiters]) wake();
  }
  #deliver(listener: (state: QueryState<readonly T[]>) => void, state: QueryState<readonly T[]>): void {
    notifyObserver(listener, () => {
      this.#checkIdentity();
      if (this.#state === state && this.#listeners.has(listener)) listener(state);
    });
  }
  #schedule(): void {
    if (this.#queued) return;
    this.#queued = true;
    afterBatch(() => { this.#queued = false; this.#recompute(); });
  }
  #recompute(): void {
    if (this.#disposed) return;
    const failed = this.#slots.find(slot => slot.state.status === "error" && protectedFailure(slot.state.error))
      ?? this.#slots.find(slot => slot.state.status === "error");
    if (failed) {
      const error = (failed.state as { error: unknown }).error;
      const retained = failed === this.#appending && !protectedFailure(error) ? this.#appendPrevious : undefined;
      this.#publish({ status: "error", error: (failed.state as { error: unknown }).error, ...(retained === undefined ? {} : { data: retained, isStale: true }) }); return;
    }
    if (this.#slots.some(slot => slot.state.status !== "success")) {
      this.#publish({ status: "pending", ...(this.data === undefined ? {} : { data: this.data }), isStale: this.isStale, isOptimistic: this.isOptimistic }); return;
    }
    const rows = this.#slots.flatMap(slot => (slot.state as { data: ReadonlyResult<Page<T>> }).data.page) as ReadonlyResult<readonly T[]>;
    if (rows.length > 10000 || new TextEncoder().encode(JSON.stringify(rows, (_, value) => typeof value === "bigint" ? String(value) : value)).byteLength > 16 * 1024 * 1024) {
      this.#publish({ status: "error", error: new DatabaseError({ code: "execution_limit", message: "Paginated list exceeds its row or byte budget" }) }); return;
    }
    Object.freeze(rows);
    this.#publish({ status: "success", data: rows, isStale: this.#slots.some(slot => slot.state.isStale), isOptimistic: this.#slots.some(slot => slot.state.isOptimistic) });
  }
  next(pages = 1): Promise<void> {
    this.#checkIdentity();
    if (!Number.isSafeInteger(pages) || pages < 1 || pages > 128) return Promise.reject(new TypeError("pages must be an integer from 1 to 128"));
    if (this.#loading) return this.#loading;
    if (this.#disposed) return Promise.reject(this.error ?? new DatabaseError({ code: "disposed", message: "Pagination handle is disposed" }));
    const load = async () => {
      for (let count = 0; count < pages; count++) {
        if (this.#disposed) throw this.error;
        if (this.#encode(this.#mapper(null)) !== this.#baseline) throw new TypeError("Pagination inputs changed; create a fresh handle");
        if (this.#slots.length && !this.hasMore) return;
        if (this.#slots.length >= 128) throw new DatabaseError({ code: "capacity", message: "Pagination exceeds 128 pages" });
        const previous = this.data;
        const last = this.#slots.at(-1)?.state;
        const cursor = last?.status === "success" ? last.data.cursor : null;
        const input = this.#mapper(cursor);
        const inputKey = this.#encode(input);
        if (this.#slots.some(loaded => loaded.inputKey === inputKey)) {
          throw new TypeError("Pagination mapper must forward the continuation cursor to a new page input");
        }
        const slot: Slot<T> = { inputKey, operation: { name: this.#name, kind: "query", input, ...(this.#codecs ? { codecs: this.#codecs } : {}), page: { owner: `${this.#owner}:${this.#slots.length}` } }, state: { status: "pending" } };
        this.#appending = slot; this.#appendPrevious = previous;
        this.#slots.push(slot);
        this.#publish({ status: "pending", ...(previous === undefined ? {} : { data: previous }) });
        try {
          const outcome = new Promise<void>((resolve, reject) => {
            const check = () => {
              if (this.#disposed || slot.state.status === "error") { this.#waiters.delete(check); reject(this.#disposed ? this.error : (slot.state as { error: unknown }).error); }
              else if (slot.state.status === "success") { this.#waiters.delete(check); resolve(); }
            };
            this.#waiters.add(check);
            try {
              let discovered = false;
              const stopWatch = this.#runtime.watch!(slot.operation, stagedObserver(state => {
                let next = querySnapshot<Page<T>>(state, this.#codecs);
                if (!discovered && next.status === "success") {
                  discovered = true;
                  if (next.data.cursor !== null && next.data.cursor === cursor) {
                    next = { status: "error", error: new TypeError("Pagination result cursor did not advance") };
                  }
                }
                slot.state = next;
                this.#schedule();
              }), {});
              if (this.#disposed) stopWatch();
              else slot.stop = stopWatch;
            } catch (error) {
              this.#waiters.delete(check);
              reject(error);
            }
          });
          await outcome;
          if (this.#state.status === "error") throw this.#state.error;
        } catch (error) {
          const stopWatch = slot.stop;
          delete slot.stop;
          stopWatch?.();
          this.#slots.pop();
          const refreshFailure = this.#slots.find(loaded => loaded.state.status === "error");
          if (!this.#disposed) {
            if (protectedFailure(error)) this.#publish({ status: "error", error });
            else if (refreshFailure) this.#recompute();
            else this.#publish({ status: "error", error, ...(previous === undefined || protectedFailure(error) ? {} : { data: previous, isStale: true }) });
          }
          throw error;
        } finally { this.#appending = undefined; this.#appendPrevious = undefined; }
      }
    };
    this.#loading = Promise.resolve().then(load).catch(error => {
      if (!this.#disposed && !(this.#state.status === "error" && (this.#state.error === error || protectedFailure(this.#state.error)))) {
        const previous = this.data;
        this.#publish({ status: "error", error, ...(previous === undefined ? {} : { data: previous, isStale: true }) });
      }
      throw error;
    }).finally(() => { this.#loading = undefined; });
    return this.#loading;
  }
  subscribe(listener: (state: QueryState<readonly T[]>) => void, options: QuerySubscriptionOptions = {}): () => void {
    options = subscriptionOptions(options); options.signal?.throwIfAborted();
    if (typeof listener !== "function") throw new TypeError("Pagination subscription needs a listener");
    this.#checkIdentity();
    const deliver = queryListener(listener, options);
    if (this.#disposed) { deliver(this.#state); return () => {}; }
    this.#listeners.add(deliver);
    this.#deliver(deliver, this.#state);
    const stop = () => {
      options.signal?.removeEventListener("abort", stop);
      this.#listeners.delete(deliver);
      if (!this.#listeners.size) this.dispose();
    };
    options.signal?.addEventListener("abort", stop, { once: true });
    // The initial callback may abort or dispose synchronously before registration.
    if (options.signal?.aborted || this.#disposed) stop();
    return stop;
  }
  #retire(error: unknown): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#identitySignal?.removeEventListener("abort", this.#abort);
    for (const slot of this.#slots) {
      const stopWatch = slot.stop;
      delete slot.stop;
      stopWatch?.();
    }
    this.#slots = [];
    this.#publish({ status: "error", error });
    // Terminal delivery must finish before membership is retired, including
    // when auth changes inside an adapter notification transaction.
    notification(() => this.#listeners.clear());
  }
  dispose(): void { this.#retire(new DatabaseError({ code: "disposed", message: "Pagination handle is disposed" })); }
}
