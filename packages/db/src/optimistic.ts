import type { Operation } from "./contracts.ts";

/** Internal cache bridge used by generated withOptimistic wrappers. */
export interface PredictionQuery { readonly name: string; readonly input?: unknown }
export interface PendingPrediction { readonly requestId: string; readonly name: string; readonly input: unknown }
export interface PredictionContext {
  readonly requestId: string;
  get(query: PredictionQuery): unknown | undefined;
  set(query: PredictionQuery, result: unknown): void;
  queries(name: string): readonly PredictionQuery[];
}
export type Predictor = (input: unknown, context: PredictionContext) => void;
interface Base { operation: Operation; data: unknown; source: string; revision: bigint }
interface Layer extends PendingPrediction {
  predictor: Predictor;
  targets?: Set<string>;
  fence?: { source: string; revision: bigint };
}
const copy = <T>(value: T): T => structuredClone(value);
function warn(): void {
  if (typeof process !== "undefined" && process.env.NODE_ENV === "development") console.warn("@hulla/db: optimistic update failed; its cache changes were removed");
}

/** Predictions replay in submission order over the latest server base. Each
 * invocation publishes all of its target updates together or none of them. */
export class OptimisticResults {
  readonly #resolve: (query: PredictionQuery) => { key: string; operation: Operation };
  readonly #base = new Map<string, Base>();
  readonly #layers = new Map<string, Layer>();
  readonly #limit: number;

  constructor(resolve: (query: PredictionQuery) => { key: string; operation: Operation }, limit = 128) {
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > 1024) throw new TypeError("Invalid optimistic layer budget");
    this.#resolve = resolve; this.#limit = limit;
  }
  confirm(key: string, operation: Operation, data: unknown, source: string, revision: string): void {
    this.confirmRound([{ key, operation, data, source, revision }]);
  }
  confirmRound(results: readonly { key: string; operation: Operation; data: unknown; source: string; revision: string }[]): void {
    for (const { key, operation, data, source, revision } of results) {
      const previous = this.#base.get(key), sequence = BigInt(revision);
      if (previous && previous.source === source && previous.revision > sequence) continue;
      this.#base.set(key, { operation, data: copy(data), source, revision: sequence });
    }
    this.#rebuild();
  }

  removeResult(key: string): void {
    this.#base.delete(key);
    for (const [id, layer] of this.#layers) {
      layer.targets?.delete(key);
      if (!layer.targets?.size) this.#layers.delete(id);
    }
  }
  clear(): void { this.#base.clear(); this.#layers.clear(); }
  begin(requestId: string, name: string, input: unknown, predictor: Predictor): void {
    if (this.#layers.size >= this.#limit) throw new Error("Optimistic layer budget exceeded");
    if (this.#layers.has(requestId)) throw new Error("Duplicate optimistic request ID");
    this.#layers.set(requestId, { requestId, name, input: copy(input), predictor });
    this.#rebuild();
  }
  commit(requestId: string, source: string, revision: string): void {
    const layer = this.#layers.get(requestId);
    if (!layer) return;
    if (layer.fence && (layer.fence.source !== source || layer.fence.revision !== BigInt(revision))) throw new Error("Conflicting optimistic receipt fence");
    layer.fence = { source, revision: BigInt(revision) };
    this.#rebuild();
  }
  reject(requestId: string): void { this.#layers.delete(requestId); this.#rebuild(); }
  queries(requestId: string): readonly Operation[] {
    const layer = this.#layers.get(requestId);
    return layer ? [...layer.targets ?? []].filter(key => this.#base.has(key) && !this.#visible(layer, key)).map(key => this.#base.get(key)!.operation) : [];
  }
  unconfirmed(): readonly PendingPrediction[] {
    return [...this.#layers.values()].filter(layer => !layer.fence).map(({ requestId, name, input }) => ({ requestId, name, input: copy(input) }));
  }
  view(key: string): { data: unknown; isOptimistic: boolean } | undefined {
    const views = this.#rebuild();
    if (!views.has(key)) return undefined;
    const isOptimistic = [...this.#layers.values()].some(layer => layer.targets?.has(key) && !this.#visible(layer, key));
    return { data: copy(views.get(key)), isOptimistic };
  }
  #visible(layer: Layer, key: string): boolean {
    const base = this.#base.get(key);
    return !!base && !!layer.fence && base.source === layer.fence.source && base.revision >= layer.fence.revision;
  }
  #rebuild(): Map<string, unknown> {
    const views = new Map([...this.#base].map(([key, base]) => [key, copy(base.data)]));
    for (const [id, layer] of this.#layers) {
      if (layer.fence && [...layer.targets ?? []].every(key => this.#visible(layer, key))) {
        this.#layers.delete(id); continue;
      }
      const staged = new Map<string, unknown>();
      // Enumeration preserves each owned cache entry without exposing cache keys
      // or pagination range metadata to application selectors.
      const identities = new WeakMap<PredictionQuery, { key: string; operation: Operation }>();
      const resolve = (query: PredictionQuery) => identities.get(query) ?? this.#resolve(query);
      const eligible = (key: string) => this.#base.has(key) && (!layer.targets || layer.targets.has(key));
      try {
        const result: unknown = layer.predictor(copy(layer.input), {
          requestId: layer.requestId,
          get: query => {
            const { key } = resolve(query);
            return eligible(key) ? copy(staged.has(key) ? staged.get(key) : views.get(key)) : undefined;
          },
          set: (query, value) => {
            const { key, operation } = resolve(query);
            if (!eligible(key)) return;
            if (!operation.codecs) throw new Error("Prediction requires generated result validation");
            let parsed = operation.codecs.result.parse(copy(value));
            if (operation.page) {
              const base = this.#base.get(key)!.data as { cursor: unknown };
              // Continuation belongs to the confirmed range, never a prediction.
              parsed = { ...(parsed as object), cursor: base.cursor };
            }
            staged.set(key, parsed);
          },
          queries: name => [...this.#base].filter(([key, base]) => base.operation.name === name && eligible(key))
            .map(([key, base]) => {
              const query = { name, input: copy(base.operation.input) };
              identities.set(query, { key, operation: base.operation });
              return query;
            }),
        });
        if (result !== undefined) {
          if (typeof (result as PromiseLike<unknown>)?.then === "function") void Promise.resolve(result).catch(() => {});
          throw new TypeError("Optimistic updaters must be synchronous");
        }
        // The target set is fixed at invocation; missing results cannot receive
        // this prediction later merely because they enter the cache.
        layer.targets ??= new Set(staged.keys());
        if (!layer.targets.size) { this.#layers.delete(id); continue; }
        for (const [key, value] of staged) if (!this.#visible(layer, key)) views.set(key, value);
      } catch {
        this.#layers.delete(id); warn();
      }
    }
    return views;
  }
}
