import type { Operation } from "./contracts.ts";
import type { FunctionCodecs } from "./codec.ts";
import type { ClientStorage, QueuedMutation, StoredSession } from "./client-storage.ts";
import { DatabaseError, applicationError, isApplicationError } from "./error.ts";
import type { OfflineResolution, OfflineResolver } from "./offline.ts";
import type { Receipt } from "./http/protocol.ts";

export interface QueueSession extends StoredSession { readonly ticket: string }
export interface QueueGateway {
  context(): { readonly key: string; readonly signal: AbortSignal };
  session(): Promise<QueueSession>;
  contract(name: string): FunctionCodecs;
  send(intent: QueuedMutation, session: QueueSession, recovering: boolean, receipt: (receipt: Receipt) => Promise<void>, onStatus?: (status: "recovering") => void): Promise<unknown>;
}
type Failure = { code: string; message: string; details?: import("./error.ts").DatabaseErrorDetails; application?: boolean };
const encodedError = (error: DatabaseError): Failure => ({ ...error.toJSON(), ...(isApplicationError(error) ? { application: true } : {}) });
function decodedError(value: unknown): DatabaseError {
  const failure = value as Failure;
  if (!failure || typeof failure.code !== "string" || typeof failure.message !== "string") return new DatabaseError({ code: "offline_conflict", message: "Queued mutation needs an explicit conflict decision" });
  return failure.application ? applicationError(failure) : new DatabaseError(failure);
}
function transient(error: unknown): boolean {
  return !(error instanceof DatabaseError) || (!isApplicationError(error) && ["unavailable", "capacity", "outcome_unknown", "confirmation_timeout", "confirmation_interrupted"].includes(error.code));
}
function waitFor<T>(value: PromiseLike<T> | T, signal: AbortSignal): Promise<T> {
  return new Promise((resolve, reject) => {
    const aborted = () => reject(signal.reason);
    if (signal.aborted) { reject(signal.reason); return; }
    signal.addEventListener("abort", aborted, { once: true });
    Promise.resolve(value).then(result => { signal.removeEventListener("abort", aborted); resolve(result); }, error => { signal.removeEventListener("abort", aborted); reject(error); });
  });
}

/** Durable intent queue. A sent attempt is recovered by receipt lookup, never resubmitted. */
export class OfflineQueue {
  readonly #storage: ClientStorage;
  readonly #gateway: QueueGateway;
  readonly #resolvers = new Map<string, OfflineResolver<unknown>>();
  readonly #waiters = new Map<string, { resolve(value: unknown): void; reject(error: unknown): void; onStatus?: (status: "recovering") => void; predict?: (id: string, input: unknown) => void; predicted?: string }>();
  readonly #deferred = new Set<string>();
  #flushing: Promise<void> | undefined;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #stopped = false;

  constructor(storage: ClientStorage, gateway: QueueGateway) { this.#storage = storage; this.#gateway = gateway; }
  register(name: string, resolver: OfflineResolver<unknown>): void {
    const previous = this.#resolvers.get(name);
    if (previous && previous !== resolver) throw new TypeError(`Conflicting offline resolvers for ${name}`);
    this.#resolvers.set(name, resolver);
    this.schedule(0);
  }
  async call(operation: Operation, resolver: OfflineResolver<unknown>, onStatus?: (status: "recovering") => void, predict?: (id: string, input: unknown) => void): Promise<unknown> {
    this.register(operation.name, resolver);
    const { key, signal } = this.#gateway.context();
    await this.#storage.activate(key);
    let owner = await this.#storage.session(key);
    if (!owner) {
      let verified: QueueSession;
      try { verified = await this.#gateway.session(); }
      catch (error) {
        if (transient(error)) throw new DatabaseError({ code: "offline_uninitialized", message: "Connect to this source once before queuing calls offline" });
        throw error;
      }
      owner = { source: verified.source, scope: verified.scope, principal: verified.principal, authorizationScope: verified.authorizationScope };
    }
    signal.throwIfAborted();
    const id = crypto.randomUUID();
    const intent: QueuedMutation = { id, requestId: id, name: operation.name, input: operation.codecs!.input.encode(operation.input), owner, attempt: 1, state: "queued" };
    await this.#storage.enqueue(key, intent);
    signal.throwIfAborted();
    return new Promise((resolve, reject) => {
      const aborted = () => { this.#waiters.delete(id); reject(new DatabaseError({ code: "confirmation_interrupted", message: "The queued call was retained after its client scope ended", details: { requestId: id } })); };
      signal.addEventListener("abort", aborted, { once: true });
      this.#waiters.set(id, { resolve: value => { signal.removeEventListener("abort", aborted); resolve(value); }, reject: error => { signal.removeEventListener("abort", aborted); reject(error); }, ...(onStatus ? { onStatus } : {}), ...(predict ? { predict } : {}) });
      this.#predict(intent, operation.input);
      if (signal.aborted) aborted(); else this.schedule(0);
    });
  }
  schedule(delay = 250): void {
    if (this.#waiters.size) this.#timer?.ref?.();
    if (this.#stopped || this.#timer || this.#flushing) return;
    this.#timer = setTimeout(() => { this.#timer = undefined; void this.flush().catch(() => {}); }, delay);
    if (!this.#waiters.size) this.#timer.unref?.();
  }
  flush(): Promise<void> {
    if (this.#flushing) return this.#flushing;
    if (this.#stopped) return Promise.resolve();
    let retry = false;
    this.#flushing = this.#drain().then(value => { retry = value; }).catch(error => {
      for (const id of [...this.#waiters.keys()]) this.#settle(id, undefined, new DatabaseError({ code: "offline_storage_failed", message: "The queued call was retained after client storage failed", details: { requestId: id } }));
      throw error;
    }).finally(() => {
      this.#flushing = undefined;
      if (retry) this.schedule();
    });
    return this.#flushing;
  }
  async #drain(): Promise<boolean> {
    const { key, signal } = this.#gateway.context();
    while (!this.#stopped && !signal.aborted) {
      const intents = await this.#storage.mutations(key);
      for (const completed of intents) if (this.#waiters.has(completed.id)) {
        if (completed.state === "committed") {
          try { this.#settle(completed.id, this.#gateway.contract(completed.name).result.decode(completed.result)); }
          catch { this.#settle(completed.id, undefined, new DatabaseError({ code: "invalid_result", message: "The committed result was retained but no longer matches its contract", details: { requestId: completed.id, committed: true } })); }
        }
        else if (completed.state === "failed") this.#settle(completed.id, undefined, decodedError(completed.error));
      }
      const intent = intents.find(intent => intent.state !== "committed" && intent.state !== "failed");
      if (!intent || this.#deferred.has(intent.id) || !this.#resolvers.has(intent.name)) return false;
      let current = intent;
      let codecs: FunctionCodecs;
      let input: unknown;
      try { codecs = this.#gateway.contract(intent.name); input = codecs.input.decode(intent.input); }
      catch {
        this.#deferred.add(intent.id);
        this.#settle(intent.id, undefined, new DatabaseError({ code: "contract_changed", message: "The queued call no longer matches its function contract and was retained", details: { requestId: intent.id } }));
        return false;
      }
      try {
        if (intent.state === "queued") this.#predict(intent, input);
        if (intent.state === "conflict") {
          const resolver = this.#resolvers.get(intent.name)!;
          const resolution = await waitFor(resolver({ requestId: intent.id, input, error: decodedError(intent.error), attempt: intent.attempt }), signal);
          signal.throwIfAborted();
          await this.resolve(intent.id, resolution);
          continue;
        }
        const session = await this.#gateway.session();
        signal.throwIfAborted();
        if (session.source !== intent.owner.source || session.scope !== intent.owner.scope || session.principal !== intent.owner.principal
          || (intent.state === "sent" && session.authorizationScope !== intent.owner.authorizationScope)) throw new DatabaseError({ code: "context_changed", message: "The verified owner of a queued call changed; the original attempt was retained" });
        if (intent.state === "queued") {
          current = { ...intent, state: "sent", ticket: session.ticket, owner: { source: session.source, scope: session.scope, principal: session.principal, authorizationScope: session.authorizationScope } };
          if (!await this.#storage.update(key, current, intent)) continue;
        }
        const result = await this.#gateway.send(current, session, intent.state === "sent", async receipt => {
          const committed: QueuedMutation = { ...current, state: "committed", result: receipt.result };
          if (!await this.#storage.update(key, committed, current)) {
            const installed = (await this.#storage.mutations(key)).find(value => value.id === current.id);
            if (installed?.state !== "committed") throw new DatabaseError({ code: "outcome_unknown", message: "Could not persist the mutation receipt" });
          }
        }, this.#waiters.get(intent.id)?.onStatus);
        this.#settle(intent.id, result);
      } catch (error) {
        if (signal.aborted || this.#stopped) return false;
        if (intent.state === "conflict") {
          this.#deferred.add(intent.id);
          this.#settle(intent.id, undefined, new DatabaseError({ code: "offline_resolution_failed", message: "The conflict callback failed; the queued call was retained", details: { requestId: intent.id } }));
          return false;
        }
        if (error instanceof DatabaseError && error.code === "receipt_expired") {
          await this.#storage.update(key, { ...current, state: "failed", error: encodedError(error) }, current);
          this.#settle(intent.id, undefined, error);
          continue;
        }
        if (transient(error) || (error instanceof DatabaseError && typeof error.details === "object" && error.details !== null && !Array.isArray(error.details) && "committed" in error.details && error.details.committed === true)) return true;
        const failure = error instanceof DatabaseError ? error : new DatabaseError({ code: "offline_failed", message: "The queued call failed" });
        if (current.state === "sent" && (isApplicationError(failure) || ["conflict", "row_expectation_failed"].includes(failure.code))) {
          await this.#storage.update(key, { ...current, state: "conflict", error: encodedError(failure) }, current);
          continue;
        }
        this.#deferred.add(intent.id);
        this.#settle(intent.id, undefined, failure);
        return false;
      }
    }
    return false;
  }
  #settle(id: string, result: unknown, error?: unknown): void {
    const waiter = this.#waiters.get(id); this.#waiters.delete(id);
    if (error) waiter?.reject(error); else waiter?.resolve(result);
  }
  #predict(intent: QueuedMutation, input: unknown): void {
    const waiter = this.#waiters.get(intent.id);
    if (!waiter?.predict || waiter.predicted === intent.requestId) return;
    waiter.predicted = intent.requestId;
    waiter.predict(intent.requestId, input);
  }
  async pending(): Promise<readonly QueuedMutation[]> { return this.#storage.mutations(this.#gateway.context().key); }
  async acknowledge(id: string): Promise<void> {
    const { key } = this.#gateway.context();
    const intent = (await this.#storage.mutations(key)).find(value => value.id === id);
    if (!intent || !["committed", "failed"].includes(intent.state)) throw new TypeError("Only terminal queued calls can be acknowledged");
    await this.#storage.remove(key, id);
    this.#deferred.delete(id);
  }
  async resolve(id: string, resolution: OfflineResolution<unknown>): Promise<void> {
    if (!resolution || !["retry", "reject", "defer"].includes(resolution.action)) throw new TypeError("Expected an explicit retry, reject or defer decision");
    const { key, signal } = this.#gateway.context();
    const intent = (await this.#storage.mutations(key)).find(value => value.id === id);
    if (!intent || intent.state !== "conflict") throw new TypeError("The queued call has no definitive conflict to resolve");
    signal.throwIfAborted();
    if (resolution.action === "defer") {
      this.#deferred.add(id);
      this.#settle(id, undefined, new DatabaseError({ code: "offline_conflict", message: "Conflict resolution was deferred; the queued call was retained", details: { requestId: id } }));
      return;
    }
    let next: QueuedMutation;
    if (resolution.action === "retry") {
      if (intent.attempt >= 16) throw new DatabaseError({ code: "execution_limit", message: "Offline conflict retry limit reached; the queued call was retained" });
      const { ticket: _ticket, error: _error, result: _result, ...base } = intent;
      next = { ...base, state: "queued", requestId: crypto.randomUUID(), input: this.#gateway.contract(intent.name).input.encode(resolution.input), attempt: intent.attempt + 1 };
    } else next = { ...intent, state: "failed" };
    if (!await this.#storage.update(key, next, intent)) throw new DatabaseError({ code: "context_changed", message: "The queued conflict changed while it was being resolved" });
    this.#deferred.delete(id);
    if (resolution.action === "reject") this.#settle(id, undefined, decodedError(intent.error));
    this.schedule(0);
  }
  stop(): void { this.#stopped = true; clearTimeout(this.#timer); this.#timer = undefined; }
}
