import { batchNotifications, notifyObserver } from "./notifications.ts";
import type { CallOptions, FunctionRuntime, Operation, QueryState } from "./contracts.ts";
import type { FunctionCodecs } from "./codec.ts";
import { DatabaseError, isApplicationError } from "./error.ts";
import {
  operationKey, parseError, parseSession, parseHead,
  parseReceipt, parseSnapshot, parsePage, record, sequence, uuid,
  type Session, type Snapshot, type Receipt,
} from "./http/protocol.ts";
import { OptimisticResults } from "./optimistic.ts";
import { sseEvents } from "./http/sse.ts";
import { authState, isAuthPromise, sameAuth, type ClientAuthInput, type ClientAuthState } from "./auth.ts";

export interface HttpRuntimeOptions {
  readonly baseUrl: string;
  /** App-owned cache namespace. Never sent as identity or credentials. */
  readonly auth?: ClientAuthInput;
  /** Read current credentials for each request, independently of setAuth. */
  readonly headers?: HeadersInit | (() => HeadersInit | Promise<HeadersInit>);
  readonly credentials?: RequestCredentials;
  readonly contracts: Readonly<Record<string, FunctionCodecs & { readonly kind: "query" | "mutation" }>>;
  readonly fetch?: typeof globalThis.fetch;
  readonly operationTimeoutMs?: number;
  /** Overall deadline for one-shot reads, including connection recovery. */
  readonly queryTimeoutMs?: number;
  readonly streamIdleTimeoutMs?: number;
  readonly maxResponseBytes?: number;
  readonly reconnectDelayMs?: number;
  /** Overall mutation confirmation budget, including submission and all receipt checks. */
  readonly confirmationTimeoutMs?: number;
  readonly maxOptimisticLayers?: number;
}
interface VerifiedSession extends Session { readonly source: string; readonly ticket: string; readonly expiresAt: number; readonly renewAfter: number }
interface PendingMutation { readonly requestId: string; readonly name: string; readonly input: unknown; readonly source: string; readonly ticket: string; receipt?: Receipt }
interface Watch {
  readonly operation: Operation & { readonly kind: "query" };
  readonly observers: Set<(state: QueryState<unknown>) => void>;
  readonly controller: AbortController;
  readonly cleanups: Set<() => void>;
  state: QueryState<unknown>;
}
const statuses: Record<string, number> = { unauthorized: 401, forbidden: 403, invalid_request: 400, execution_limit: 422, execution_failed: 422, row_expectation_failed: 422, quota_exceeded: 422, ticket_expired: 410, not_found: 404, conflict: 409, request_id_reused: 409, receipt_expired: 410, capacity: 429, unavailable: 503, reset_required: 409, outcome_unknown: 503 };
class TransportFailure extends Error {
  constructor(cause: unknown) { super("Database transport interrupted", { cause }); }
}
function transient(error: unknown): boolean { return !(error instanceof DatabaseError) || (!isApplicationError(error) && ["unavailable", "capacity", "outcome_unknown"].includes(error.code)); }
function copy<T>(value: T): T { return structuredClone(value); }
function budget(value: number | undefined, fallback: number): number {
  value ??= fallback;
  if (!Number.isSafeInteger(value) || value < 1 || value > 2_147_483_647) throw new TypeError("Invalid runtime budget");
  return value;
}
function pause(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    signal.throwIfAborted();
    const aborted = () => { clearTimeout(timer); reject(signal.reason); };
    const timer = setTimeout(() => { signal.removeEventListener("abort", aborted); resolve(); }, ms);
    signal.addEventListener("abort", aborted, { once: true });
  });
}

function abortable<T>(promise: Promise<T>, signal?: AbortSignal): Promise<T> {
  if (!signal) return promise;
  return new Promise((resolve, reject) => {
    const abort = () => reject(signal.reason);
    if (signal.aborted) { void promise.catch(() => {}); reject(signal.reason); return; }
    signal.addEventListener("abort", abort, { once: true });
    void promise.then(value => { signal.removeEventListener("abort", abort); resolve(value); }, error => { signal.removeEventListener("abort", abort); reject(error); });
  });
}

/** Generated function runtime. One owner coordinates in-memory exact-query subscriptions and receipt confirmation. */
export class HttpRuntime implements FunctionRuntime {
  readonly #baseUrl: string;
  readonly #options: HttpRuntimeOptions;
  readonly #fetch: typeof globalThis.fetch;
  readonly #timeout: number;
  readonly #queryTimeout: number;
  readonly #idleTimeout: number;
  readonly #maxBytes: number;
  readonly #reconnect: number;
  readonly #confirmationTimeout: number;
  readonly #optimism: OptimisticResults;
  #auth: ClientAuthState = null;
  #authPending = false;
  #authFailure: DatabaseError | undefined;
  #authUpdate = 0;
  #authTimer: ReturnType<typeof setTimeout> | undefined;
  #disposed = false;
  #epoch = 0;
  #lifetime = new AbortController();
  #source: string | undefined;
  #handshake: Promise<VerifiedSession> | undefined;
  #session: VerifiedSession | undefined;
  #renewTimer: ReturnType<typeof setTimeout> | undefined;
  #pending = new Map<string, PendingMutation>();
  #watches = new Map<string, Watch>();
  #results = new Map<string, Pick<Snapshot, "source" | "revision"> & { bytes: number }>();
  #calls = new Set<Promise<unknown>>();
  #idleResults = new Map<string, { expiresAt: number; bytes: number }>();
  #cacheTimer: ReturnType<typeof setTimeout> | undefined;
  #streamGeneration = 0;
  #liveRevision = -1n;
  #streamController: AbortController | undefined;
  #rebuildQueued = false;

  constructor(options: HttpRuntimeOptions) {
    const url = new URL(options.baseUrl);
    if (url.username || url.password || url.search || url.hash || !(url.protocol === "https:" || (url.protocol === "http:" && ["127.0.0.1", "[::1]", "localhost"].includes(url.hostname)))) throw new TypeError("Use HTTPS or loopback HTTP without URL credentials");
    this.#baseUrl = url.href.replace(/\/$/, "");
    this.#options = { ...options, contracts: Object.freeze({ ...options.contracts }) };
    this.#optimism = new OptimisticResults(query => {
      const operation = this.#operation({ name: query.name, kind: "query", input: query.input });
      return { key: operationKey(operation), operation };
    }, options.maxOptimisticLayers);
    // Browser fetch checks its receiver; calling it as a runtime method is invalid.
    this.#fetch = (options.fetch ?? globalThis.fetch).bind(globalThis);
    this.#timeout = budget(options.operationTimeoutMs, 10_000);
    this.#queryTimeout = budget(options.queryTimeoutMs, 30_000);
    this.#idleTimeout = budget(options.streamIdleTimeoutMs, 30_000);
    this.#maxBytes = budget(options.maxResponseBytes, 16 * 1024 * 1024 + 64 * 1024);
    this.#reconnect = budget(options.reconnectDelayMs, 250);
    this.#confirmationTimeout = budget(options.confirmationTimeoutMs, 30_000);
    this.setAuth(options.auth ?? null);
  }

  get identityEpoch(): number { this.#expireAuth(); return this.#epoch; }
  get identitySignal(): AbortSignal { this.#expireAuth(); return this.#lifetime.signal; }
  get clientAuth() {
    this.#expireAuth();
    return { key: this.#auth?.key ?? null, pending: this.#authPending,
      ...(this.#authFailure ? { error: this.#authFailure } : {}) };
  }
  #expireAuth(): boolean {
    if (!this.#disposed && this.#auth !== null
      && this.#auth.expiresAt !== undefined && this.#auth.expiresAt <= Date.now()) {
      clearTimeout(this.#authTimer); this.#authTimer = undefined;
      this.#auth = null;
      this.#retire(new DatabaseError({ code: "context_changed", message: "Function auth scope expired" }));
      return true;
    }
    return false;
  }
  #scheduleAuthExpiry(): void {
    clearTimeout(this.#authTimer); this.#authTimer = undefined;
    if (this.#expireAuth()) return;
    if (this.#disposed || this.#auth === null || this.#auth.expiresAt === undefined) return;
    // Long deadlines must not overflow setTimeout. Recheck the wall clock when
    // waking; access checks also fence results after a suspended browser resumes.
    this.#authTimer = setTimeout(() => this.#scheduleAuthExpiry(), Math.min(Math.max(1, this.#auth.expiresAt - Date.now()), 2_147_483_647));
    this.#authTimer.unref?.();
  }
  #check(epoch: number): void {
    this.#expireAuth();
    if (this.#disposed) throw new DatabaseError({ code: "disposed", message: "Function runtime disposed" });
    if (epoch !== this.#epoch || this.#lifetime.signal.aborted) throw new DatabaseError({ code: "context_changed", message: "Function auth scope retired" });
    if (this.#authPending) throw new DatabaseError({ code: "authentication_pending", message: "Authentication is still loading" });
    if (this.#authFailure) throw this.#authFailure;
  }
  #contract(operation: Pick<Operation, "name" | "kind">): FunctionCodecs {
    const contract = Object.hasOwn(this.#options.contracts, operation.name) ? this.#options.contracts[operation.name] : undefined;
    if (!contract || contract.kind !== operation.kind) throw new TypeError("Unknown function or execution kind");
    return contract;
  }
  #operation(operation: Operation): Operation {
    const codecs = this.#contract(operation);
    return { name: operation.name, kind: operation.kind, input: codecs.input.parse(operation.input), codecs, ...(operation.page ? { page: operation.page } : {}) };
  }
  #snapshot(value: unknown, session: Session, source?: string): Snapshot {
    const snapshot = parseSnapshot(value);
    if (snapshot.scope !== session.scope || snapshot.principal !== session.principal || snapshot.authorization_scope !== session.authorizationScope) throw new DatabaseError({ code: "reset_required", message: "Response identity does not match the active session" });
    if (source && source !== snapshot.source) throw new DatabaseError({ code: "reset_required", message: "reset_required" });
    return snapshot;
  }

  async #headers(session: Session | undefined, signal: AbortSignal, accept: string): Promise<Headers> {
    this.#expireAuth();
    signal.throwIfAborted();
    const configured = this.#options.headers;
    const value = typeof configured === "function" ? await abortable(Promise.resolve(configured()), signal) : configured;
    this.#expireAuth();
    signal.throwIfAborted();
    const headers = new Headers(value);
    // Identity pins come exclusively from the verified server handshake.
    for (const key of [...headers.keys()]) if (key.startsWith("x-hulladb-")) headers.delete(key);
    headers.set("content-type", "application/json"); headers.set("accept", accept);
    headers.delete("last-event-id");
    if (session) {
      headers.set("x-hulladb-scope", session.scope);
      headers.set("x-hulladb-principal", session.principal);
      headers.set("x-hulladb-authorization-scope", session.authorizationScope);
    }
    return headers;
  }
  async #json(path: string, body: unknown, session: Session | undefined, signal: AbortSignal): Promise<unknown> {
    const deadline = new AbortController();
    const timer = setTimeout(() => deadline.abort(new Error("Database request timed out")), this.#timeout);
    const combined = AbortSignal.any([signal, deadline.signal]);
    try {
      const headers = await this.#headers(session, combined, "application/json");
      combined.throwIfAborted();
      const request = this.#fetch(this.#baseUrl + path, {
        method: body === undefined ? "GET" : "POST", redirect: "error", credentials: this.#options.credentials ?? "same-origin", signal: combined,
        headers,
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      }).catch(error => { throw new TransportFailure(error); });
      void request.then(response => { if (combined.aborted) void response.body?.cancel().catch(() => {}); }, () => {});
      const response = await abortable(request, combined);
      return await this.#readResponse(response, combined);
    } catch (error) {
      if (signal.aborted) throw signal.reason;
      if (deadline.signal.aborted) throw new TransportFailure(error);
      throw error;
    } finally { clearTimeout(timer); }
  }
  async #readResponse(response: Response, signal?: AbortSignal): Promise<unknown> {
    if (response.headers.get("content-type")?.split(";")[0] !== "application/json" || !response.body) { void response.body?.cancel().catch(() => {}); throw new TypeError("Invalid JSON response"); }
    const reader = response.body.getReader();
    let bytes = 0;
    const chunks: Uint8Array[] = [];
    try {
      while (true) {
        const chunk = await abortable(reader.read().catch(error => { throw new TransportFailure(error); }), signal);
        if (chunk.done) break;
        bytes += chunk.value.byteLength;
        if (bytes > this.#maxBytes) throw new TypeError("Response exceeds budget");
        chunks.push(chunk.value);
      }
    } finally { void reader.cancel().catch(() => {}); reader.releaseLock(); }
    const joined = new Uint8Array(bytes); let offset = 0;
    for (const chunk of chunks) { joined.set(chunk, offset); offset += chunk.length; }
    const parsed: unknown = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(joined));
    if (!response.ok) {
      const error = parseError(parsed);
      if ((isApplicationError(error) ? 422 : statuses[error.code]) !== response.status && !(!isApplicationError(error) && error.code === "invalid_request" && [413, 415, 422].includes(response.status))) throw new TypeError("Error status does not match its contract");
      throw error;
    }
    return parsed;
  }
  async #ensure(epoch: number): Promise<VerifiedSession> {
    this.#check(epoch);
    if (this.#session && this.#session.expiresAt > Date.now()) {
      this.#scheduleRenewal(epoch);
      return this.#session;
    }
    const session = await this.#renew(epoch);
    this.#scheduleRenewal(epoch);
    return session;
  }
  #scheduleRenewal(epoch: number): void {
    clearTimeout(this.#renewTimer);
    if (!this.#session) return;
    this.#renewTimer = setTimeout(() => {
      this.#renewTimer = undefined;
      void this.#renew(epoch).then(() => {
        if (epoch === this.#epoch && (this.#calls.size || this.#watches.size)) this.#scheduleRenewal(epoch);
      }).catch(error => {
        if (error instanceof DatabaseError && !isApplicationError(error) && ["unauthorized", "forbidden", "reset_required"].includes(error.code)) void this.#invalidate(error, epoch);
      });
    }, Math.min(Math.max(1, this.#session.renewAfter - Date.now()), 2_147_483_647));
    this.#renewTimer.unref?.();
  }
  async #renew(epoch: number): Promise<VerifiedSession> {
    this.#check(epoch);
    if (!this.#handshake) {
      const previous = this.#session;
      // Measure validity conservatively from request start, including network time.
      const started = Date.now();
      this.#handshake = (async () => {
        const head = parseSession(await this.#json("/functions/session", undefined, previous, this.#lifetime.signal));
        this.#check(epoch);
        if ((this.#source && this.#source !== head.source) || (previous && (previous.scope !== head.scope
          || previous.principal !== head.principal || previous.authorizationScope !== head.authorization_scope))) throw new DatabaseError({ code: "reset_required", message: "Session renewal changed the verified scope" });
        const session = { source: head.source, scope: head.scope, principal: head.principal, authorizationScope: head.authorization_scope,
          ticket: head.ticket.id, expiresAt: started + head.ticket.expires_in_ms, renewAfter: started + head.ticket.renew_after_ms };
        if (session.expiresAt <= Date.now()) throw new DatabaseError({ code: "unavailable", message: "Session ticket expired during setup" });
        this.#source = head.source; this.#session = session;
        return session;
      })().finally(() => { if (epoch === this.#epoch) this.#handshake = undefined; });
    }
    const session = await this.#handshake;
    return session;
  }
  #publish(watch: Watch, state: QueryState<unknown>): void {
    if (state.status === "success") {
      const rendered = this.#optimism.view(operationKey(watch.operation));
      state = { ...state, ...(rendered ?? { isOptimistic: false }) };
    }
    watch.state = state;
    for (const observer of [...watch.observers]) {
      if (watch.state !== state) break;
      // An observer exception cannot break receipt recovery or another observer.
      notifyObserver(observer, () => {
        if (watch.state !== state || !watch.observers.has(observer)) return;
        try { observer(state.status === "success" ? { ...state, data: copy(state.data) } : state); } catch { /* application observer */ }
      });
    }
  }
  #refreshPredictions(): void {
    batchNotifications(() => { for (const watch of this.#watches.values()) if (watch.state.status === "success") this.#publish(watch, watch.state); });
  }
  async #correlate(snapshot: Snapshot, session: Session, epoch: number, signal: AbortSignal): Promise<void> {
    // A frame can arrive before its acknowledgement. Receipt lookup after the
    // snapshot either proves absence or supplies the fence for safe rebasing.
    for (const prediction of this.#optimism.unconfirmed()) {
      const pending = this.#pending.get(prediction.requestId);
      if (!pending) continue;
      let receipt = pending.receipt;
      if (!receipt) {
        try {
          const raw = await this.#json(`/functions/receipts/${pending.requestId}?source=${pending.source}&ticket=${pending.ticket}`, undefined, session, signal);
          this.#check(epoch);
          receipt = pending.receipt ?? (raw === null ? undefined : parseReceipt(raw, pending.requestId, session, snapshot.source));
        } catch (error) {
          this.#check(epoch);
          // ACK can compact the remote payload while this lookup is in flight.
          // The delivered local receipt remains sufficient for its optimistic fence.
          if (pending.receipt && error instanceof DatabaseError && !isApplicationError(error) && error.code === "receipt_expired") receipt = pending.receipt;
          else if (error instanceof DatabaseError && !isApplicationError(error) && ["execution_failed", "quota_exceeded", "ticket_expired", "receipt_expired"].includes(error.code)) {
            this.#optimism.reject(pending.requestId); continue;
          } else throw error;
        }
      }
      if (!receipt) continue;
      this.#contract({ name: pending.name, kind: "mutation" }).result.decode(receipt.result);
      this.#optimism.commit(pending.requestId, receipt.source, receipt.revision);
    }
  }

  // Only inactive results consume this disposable cache. Active round members
  // have their own admission/frame limits and must never be partially evicted.
  #remember(key: string, snapshot: Snapshot): void {
    this.#results.set(key, { source: snapshot.source, revision: snapshot.revision,
      bytes: new TextEncoder().encode(key + JSON.stringify(snapshot)).byteLength });
  }
  #retainIdle(key: string): void {
    const snapshot = this.#results.get(key);
    if (snapshot && !this.#watches.has(key)) {
      this.#idleResults.delete(key);
      this.#idleResults.set(key, { expiresAt: Date.now() + 5 * 60_000, bytes: snapshot.bytes });
    }
    this.#pruneCache();
  }
  #pruneCache(): void {
    clearTimeout(this.#cacheTimer); this.#cacheTimer = undefined;
    let bytes = [...this.#idleResults.values()].reduce((sum, entry) => sum + entry.bytes, 0);
    for (const [key, entry] of this.#idleResults) {
      if (entry.expiresAt > Date.now() && this.#idleResults.size <= 128 && bytes <= 16 * 1024 * 1024) break;
      this.#idleResults.delete(key); bytes -= entry.bytes;
      this.#results.delete(key); this.#optimism.removeResult(key);
    }
    const first = this.#idleResults.values().next().value;
    if (first) {
      this.#cacheTimer = setTimeout(() => this.#pruneCache(), Math.max(1, first.expiresAt - Date.now()));
      this.#cacheTimer.unref?.();
    }
  }
  #cache(key: string, snapshot: Snapshot, operation: Operation): void {
    const previous = this.#results.get(key);
    if (previous && previous.source === snapshot.source && BigInt(previous.revision) > BigInt(snapshot.revision)) return;
    this.#remember(key, snapshot);
    this.#optimism.confirm(key, operation, operation.codecs!.result.decode(snapshot.result), snapshot.source, snapshot.revision);
    this.#retainIdle(key);
    this.#refreshPredictions();
  }
  #retire(error: DatabaseError): void {
    this.#epoch++;
    this.#liveRevision = -1n;
    this.#streamGeneration++; this.#streamController?.abort();
    const lifetime = this.#lifetime;
    this.#lifetime = new AbortController();
    clearTimeout(this.#renewTimer); this.#renewTimer = undefined;
    this.#session = undefined; this.#pending.clear();
    this.#handshake = undefined; this.#source = undefined;
    const watches = [...this.#watches.values()];
    this.#watches.clear(); this.#results.clear(); this.#optimism.clear();
    this.#idleResults.clear(); clearTimeout(this.#cacheTimer); this.#cacheTimer = undefined;
    lifetime.abort(error);
    // Clear shared state before notifying: an observer may install a new auth scope.
    for (const watch of watches) {
      watch.controller.abort(error);
      this.#publish(watch, { status: "error", error });
      for (const stop of [...watch.cleanups]) stop();
    }
  }
  async #invalidate(error: unknown, epoch: number): Promise<void> {
    if (epoch !== this.#epoch) return;
    this.#retire(error instanceof DatabaseError ? error : new DatabaseError({ code: "context_changed", message: "Function auth scope retired" }));
  }

  #track<T>(call: Promise<T>): Promise<T> {
    this.#calls.add(call);
    void call.then(() => this.#calls.delete(call), () => this.#calls.delete(call));
    return call;
  }
  call(raw: Operation, options: CallOptions = {}, onStatus?: (status: "recovering") => void): Promise<unknown> {
    if (this.#calls.size >= 128) return Promise.reject(new DatabaseError({ code: "capacity", message: "Too many concurrent function calls" }));
    return this.#track(this.#call(raw, options, onStatus));
  }
  async #call(raw: Operation, options: CallOptions, onStatus?: (status: "recovering") => void): Promise<unknown> {
    options.signal?.throwIfAborted();
    const operation = this.#operation(raw), epoch = this.#epoch;
    this.#check(epoch);
    if (operation.kind === "query" && options.optimistic !== undefined) throw new TypeError("Optimistic updates apply only to mutations");
    if (operation.kind === "mutation" && options.signal !== undefined) throw new TypeError("Mutation cancellation is not supported");
    if (operation.kind === "query") return this.#query(operation, epoch, options.signal);
    const signal = options.signal ? AbortSignal.any([options.signal, this.#lifetime.signal]) : this.#lifetime.signal;
    let session: VerifiedSession;
    try { session = await abortable(this.#ensure(epoch), signal); }
    catch (error) {
      if (error instanceof DatabaseError && !isApplicationError(error) && ["unauthorized", "forbidden", "reset_required"].includes(error.code)) await this.#invalidate(error, epoch);
      throw error;
    }
    signal.throwIfAborted(); this.#check(epoch);
    const source = session.source;
    const pending: PendingMutation = { requestId: crypto.randomUUID(), name: operation.name, input: operation.codecs!.input.encode(operation.input), source, ticket: session.ticket };
    try {
      this.#check(epoch); signal.throwIfAborted();
      if (options.optimistic) {
        this.#pruneCache();
        try { this.#optimism.begin(pending.requestId, operation.name, operation.input, options.optimistic); }
        catch {
          this.#optimism.reject(pending.requestId);
          if (typeof process !== "undefined" && process.env.NODE_ENV === "development") console.warn("@hulla/db: optimistic update failed; continuing the server mutation");
        }
        this.#refreshPredictions();
        this.#check(epoch); signal.throwIfAborted();
      }
    } catch (error) {
      this.#optimism.reject(pending.requestId); this.#refreshPredictions();
      throw error;
    }
    this.#pending.set(pending.requestId, pending);
    return this.#mutation(pending, operation.codecs!, session, epoch, signal, onStatus);
  }

  async #query(operation: Operation, epoch: number, caller?: AbortSignal): Promise<unknown> {
    const deadline = new AbortController();
    const timeout = new DatabaseError({ code: "query_timeout", message: "Query did not complete within its client deadline" });
    const timer = setTimeout(() => deadline.abort(timeout), this.#queryTimeout);
    const signal = AbortSignal.any([this.#lifetime.signal, deadline.signal, ...(caller ? [caller] : [])]);
    let failures = 0;
    try {
      while (true) {
        try {
          signal.throwIfAborted(); this.#check(epoch);
          // Session setup is shared with watches and other calls. Only this
          // caller's wait is cancelled; its deadline never aborts that setup.
          const session = await abortable(this.#ensure(epoch), signal);
          signal.throwIfAborted(); this.#check(epoch);
          const source = session.source;
          const snapshot = this.#snapshot(await this.#json("/functions/query", { name: operation.name, input: operation.codecs!.input.encode(operation.input), expected_source: source, ...(operation.page?.range ? { page: { range: operation.page.range } } : {}) }, session, signal), session, source);
          this.#check(epoch);
          const data = operation.codecs!.result.decode(snapshot.result);
          if (operation.page && snapshot.page) operation.page.range = snapshot.page.range;
          const key = operationKey(operation);
          // One-shot reads cannot advance a member independently of its live round.
          if (!this.#watches.has(key)) {
            await this.#correlate(snapshot, session, epoch, signal);
            this.#check(epoch);
            if (!this.#watches.has(key)) this.#cache(key, snapshot, operation);
          }
          signal.throwIfAborted();
          return data;
        } catch (error) {
          if (signal.aborted) throw signal.reason;
          if (error instanceof DatabaseError && !isApplicationError(error) && ["unauthorized", "forbidden", "reset_required"].includes(error.code)) await this.#invalidate(error, epoch);
          if (!(error instanceof TransportFailure)) throw error;
          await pause(Math.min(this.#reconnect * 2 ** Math.min(failures++, 5), 30_000), signal);
        }
      }
    } finally { clearTimeout(timer); }
  }

  async #mutation(pending: PendingMutation, codecs: FunctionCodecs, session: Session, epoch: number, lifetime: AbortSignal, onStatus?: (status: "recovering") => void): Promise<unknown> {
    const timeout = new DatabaseError({ code: "confirmation_timeout", message: "Mutation confirmation timed out; the write may have committed", details: { requestId: pending.requestId } });
    const deadline = new AbortController();
    const timer = setTimeout(() => deadline.abort(timeout), this.#confirmationTimeout);
    const signal = AbortSignal.any([lifetime, deadline.signal]);
    let recovering = false, committed = false, failures = 0;
    try {
      while (true) {
        try {
          this.#check(epoch); signal.throwIfAborted();
          // Exactly one submission. Missing receipts are nonterminal and never
          // authorize replay of the original payload or a replacement invocation.
          const raw = recovering
            ? await this.#json(`/functions/receipts/${pending.requestId}?source=${pending.source}&ticket=${pending.ticket}`, undefined, session, signal)
            : await this.#json("/functions/mutate", { request_id: pending.requestId, ticket: pending.ticket, name: pending.name, input: pending.input, expected_source: pending.source }, session, signal);
          this.#check(epoch); signal.throwIfAborted();
          if (raw !== null || !recovering) {
            const receipt = parseReceipt(raw, pending.requestId, session, pending.source);
            committed = true;
            let result: unknown;
            try { result = codecs.result.decode(receipt.result); }
            catch { throw new DatabaseError({ code: "invalid_result", message: "The mutation committed, but its result did not match its contract", details: { requestId: pending.requestId, committed: true } }); }
            pending.receipt = receipt;
            try { this.#optimism.commit(pending.requestId, receipt.source, receipt.revision); }
            catch { this.#optimism.reject(pending.requestId); }
            this.#refreshPredictions();
            this.#check(epoch); signal.throwIfAborted();
            for (const query of this.#optimism.queries(pending.requestId)) if (!this.#watches.has(operationKey(this.#operation(query)))) void this.call(query).catch(() => {});
            // Best-effort ACKs share admission accounting so a slow endpoint
            // cannot accumulate unbounded background requests.
            void this.#track(this.#json(`/functions/receipts/${pending.requestId}/ack`, { source: pending.source, ticket: pending.ticket }, session, lifetime)).catch(() => {});
            return result;
          }
        } catch (error) {
          this.#optimism.reject(pending.requestId); this.#refreshPredictions();
          if (deadline.signal.aborted) throw timeout;
          if (epoch !== this.#epoch || lifetime.aborted) {
            throw new DatabaseError({ code: "confirmation_interrupted", message: "Mutation confirmation ended with its session; the write may have committed", details: { requestId: pending.requestId, ...(committed ? { committed: true } : {}) } });
          }
          if (error instanceof DatabaseError && !isApplicationError(error) && error.code === "receipt_expired") {
            throw new DatabaseError({ code: "receipt_expired", message: "The mutation committed, but its retained result has expired", details: { requestId: pending.requestId, committed: true } });
          }
          if (committed && !(error instanceof DatabaseError)) throw new DatabaseError({ code: "invalid_result", message: "The mutation committed, but its result could not be delivered", details: { requestId: pending.requestId, committed: true } });
          if (error instanceof DatabaseError && !transient(error)) {
            if (!isApplicationError(error) && ["unauthorized", "forbidden", "reset_required"].includes(error.code)) await this.#invalidate(error, epoch);
            throw error;
          }
          if (!recovering) {
            recovering = true;
            // State observers must never interrupt confirmation.
            try { onStatus?.("recovering"); } catch { /* application observer */ }
          }
        }
        await pause(Math.min(this.#reconnect * 2 ** Math.min(failures++, 5), 30_000), signal);
      }
    } catch (error) {
      this.#optimism.reject(pending.requestId); this.#refreshPredictions();
      if (deadline.signal.aborted) throw timeout;
      if (error instanceof DatabaseError) throw error;
      if (epoch !== this.#epoch || lifetime.aborted) throw new DatabaseError({ code: "confirmation_interrupted", message: "Mutation confirmation ended with its session; the write may have committed", details: { requestId: pending.requestId } });
      throw error;
    } finally { clearTimeout(timer); this.#pending.delete(pending.requestId); }
  }

  watch(raw: Operation & { readonly kind: "query" }, observer: (state: QueryState<unknown>) => void, options: CallOptions = {}): () => void {
    options.signal?.throwIfAborted();
    if (options.optimistic !== undefined) throw new TypeError("Optimistic updates are not a subscription option");
    const operation = this.#operation(raw) as Operation & { readonly kind: "query" };
    const epoch = this.#epoch, key = operationKey(operation);
    this.#check(epoch);
    this.#pruneCache();
    let watch = this.#watches.get(key);
    const fresh = !watch;
    if (!watch && this.#watches.size >= 128) throw new DatabaseError({ code: "capacity", message: "Live query set exceeds 128 members" });
    if (!watch) { watch = { operation, observers: new Set(), controller: new AbortController(), cleanups: new Set(), state: { status: "pending" } }; this.#watches.set(key, watch); }
    this.#idleResults.delete(key);
    const entry = watch;
    entry.observers.add(observer);
    const stop = () => {
      options.signal?.removeEventListener("abort", stop);
      entry.observers.delete(observer); entry.cleanups.delete(stop);
      if (!entry.observers.size) { entry.controller.abort(); if (this.#watches.get(key) === entry) { this.#watches.delete(key); this.#retainIdle(key); } this.#scheduleStream(); }
    };
    entry.cleanups.add(stop);
    options.signal?.addEventListener("abort", stop, { once: true });
    try { observer(entry.state.status === "success" ? { ...entry.state, data: copy(entry.state.data) } : entry.state); } catch { /* observer errors are application-owned */ }
    if (fresh && entry.observers.size && this.#watches.get(key) === entry) this.#scheduleStream();
    return stop;
  }

  #scheduleStream(): void {
    this.#streamGeneration++;
    this.#streamController?.abort();
    if (this.#rebuildQueued) return;
    this.#rebuildQueued = true;
    queueMicrotask(() => {
      this.#rebuildQueued = false;
      if (!this.#watches.size) return;
      const controller = new AbortController(); this.#streamController = controller;
      void this.#stream([...this.#watches.entries()], this.#epoch, this.#streamGeneration, controller.signal);
    });
  }
  async #stream(members: [string, Watch][], epoch: number, generation: number, membership: AbortSignal): Promise<void> {
    let failures = 0;
    let refreshedCredentials = false;
    const alive = () => epoch === this.identityEpoch && generation === this.#streamGeneration && !membership.aborted;
    while (alive()) {
      const controller = new AbortController();
      const signal = AbortSignal.any([membership, this.#lifetime.signal, controller.signal]);
      let timer: ReturnType<typeof setTimeout> | undefined;
      let verified: VerifiedSession | undefined;
      try {
        const session = await abortable(this.#ensure(epoch), signal);
        verified = session;
        const source = session.source;
        if (!alive()) return;
        timer = setTimeout(() => controller.abort(new Error("Stream setup timeout")), this.#timeout);
        const headers = await this.#headers(session, signal, "text/event-stream");

        signal.throwIfAborted();
        const request = this.#fetch(this.#baseUrl + "/functions/subscriptions", {
          method: "POST", redirect: "error", credentials: this.#options.credentials ?? "same-origin", signal,
          headers,
          body: JSON.stringify({ expected_source: source, queries: members.map(([key, watch]) => ({ key, name: watch.operation.name, input: watch.operation.codecs!.input.encode(watch.operation.input), ...(watch.operation.page?.range ? { page: { range: watch.operation.page.range } } : {}) })) }),
        });
        void request.then(response => { if (signal.aborted) void response.body?.cancel().catch(() => {}); }, () => {});
        const response = await abortable(request, signal);
        if (!response.ok) await this.#readResponse(response, signal);
        clearTimeout(timer);
        if (!response.body || response.headers.get("content-type")?.split(";")[0] !== "text/event-stream") { void response.body?.cancel().catch(() => {}); throw new TypeError("Invalid SSE response"); }
        let subscription: string | undefined, last = 0n;
        const activity = () => { clearTimeout(timer); timer = setTimeout(() => controller.abort(new Error("Stream idle timeout")), this.#idleTimeout); };
        activity();
        for await (const event of sseEvents(response.body, this.#maxBytes, activity, signal)) {
          if (!alive()) return;
          if (event.event === "error") throw parseError(JSON.parse(event.data));
          if (event.event !== "round") throw new TypeError("Unexpected SSE event");
          const frame = record(JSON.parse(event.data), ["subscription", "sequence", "round"]);
          const id = uuid(frame.subscription), next = BigInt(sequence(frame.sequence));
          if (event.id !== `${id}:${next}` || (subscription && subscription !== id) || next !== last + 1n) throw new TypeError("Invalid subscription cursor");
          const { results, ...rawHead } = record(frame.round, ["source", "scope", "principal", "authorization_scope", "revision", "results"]);
          const head = parseHead(rawHead);
          this.#snapshot({ ...head, result: null }, session, source);
          if (!Array.isArray(results) || results.length !== members.length) throw new TypeError("Incomplete live round");
          const remaining = new Map(members);
          const staged: { key: string; watch: Watch; snapshot?: Snapshot; data?: unknown; error?: DatabaseError }[] = results.map(value => {
            if (typeof value !== "object" || value === null) throw new TypeError("Invalid round member");
            const row = record(value, Object.hasOwn(value, "result") ? ["key", "result", ...(Object.hasOwn(value, "page") ? ["page"] : [])] : ["key", "error"]);
            if (typeof row.key !== "string" || !remaining.has(row.key)) throw new TypeError("Unexpected live member");
            const watch = remaining.get(row.key)!; remaining.delete(row.key);
            return { key: row.key, watch, ...(Object.hasOwn(row, "result")
              ? { snapshot: { ...head, result: row.result, ...(row.page === undefined ? {} : { page: parsePage(row.page) }) }, data: watch.operation.codecs!.result.decode(row.result) }
              : { error: parseError(row.error) }) };
          });
          for (const member of staged) {
            if (member.snapshot && member.watch.operation.page) {
              const range = member.snapshot.page?.range;
              if (!range || (member.watch.operation.page.range !== undefined && member.watch.operation.page.range !== range)) throw new TypeError("Invalid paginated range response");
            }
          }
          const discovered = staged.some(member => member.snapshot && member.watch.operation.page && !member.watch.operation.page.range);
          const credentialFailure = staged.find(member => member.error && !isApplicationError(member.error) && ["unauthorized", "reset_required"].includes(member.error.code));
          if (credentialFailure) throw credentialFailure.error;
          if (BigInt(head.revision) < this.#liveRevision) throw new TypeError("Live round revision regressed");
          await this.#correlate({ ...head, result: null }, session, epoch, signal);
          if (!alive()) return;
          subscription = id; last = next; this.#liveRevision = BigInt(head.revision);
          batchNotifications(() => {
            for (const member of staged) {
              if (member.snapshot) {
                if (member.watch.operation.page) {
                  member.watch.operation.page.range = member.snapshot.page!.range;
                }
                this.#remember(member.key, member.snapshot);
              } else { this.#results.delete(member.key); this.#optimism.removeResult(member.key); }
            }
            this.#optimism.confirmRound(staged.flatMap(member => member.snapshot ? [{ key: member.key, operation: member.watch.operation, data: member.data, source, revision: head.revision }] : []));
            for (const member of staged) {
              this.#publish(member.watch, member.snapshot
                ? { status: "success", data: member.data, isStale: false, isOptimistic: false }
                : { status: "error", error: member.error });
            }
          });
          if (discovered) { this.#scheduleStream(); return; }
          failures = 0; refreshedCredentials = false;
        }
        throw new Error("Subscription disconnected");
      } catch (error) {
        if (!alive()) return;
        // A provider may rotate a token/cookie without changing the user's cache
        // key. Reconnect with current credentials only if the server verifies the
        // same identity. Never refresh credentials ourselves or retry a mutation.
        if (error instanceof DatabaseError && !isApplicationError(error) && error.code === "unauthorized" && !refreshedCredentials && verified) {
          refreshedCredentials = true;
          clearTimeout(timer);
          try {
            const head = await this.#renew(epoch);
            this.#check(epoch);
            if (head.source !== verified.source || head.scope !== verified.scope
              || head.principal !== verified.principal || head.authorizationScope !== verified.authorizationScope) {
              throw new DatabaseError({ code: "context_changed", message: "Credential refresh changed the verified scope" });
            }
            if (alive()) continue;
            return;
          } catch (verificationError) {
            if (!alive()) return;
            // The old stream's credentials are invalid. Failure to verify a
            // replacement cannot authorize retaining its protected live state.
            error = verificationError instanceof DatabaseError && !isApplicationError(verificationError) && ["unauthorized", "forbidden", "context_changed", "reset_required"].includes(verificationError.code)
              ? verificationError : new DatabaseError({ code: "unauthorized", message: "Could not verify current credentials" });
          }
        }
        if (error instanceof DatabaseError && !isApplicationError(error) && ["unauthorized", "forbidden", "reset_required", "context_changed"].includes(error.code)) { await this.#invalidate(error, epoch); return; }
        // Keep the last complete view on transport/protocol failures; no partial
        // member is installed, and a fresh stream can recover it.
        batchNotifications(() => {
          for (const [, watch] of members) {
            if (watch.state.status === "success") this.#publish(watch, { ...watch.state, isStale: true });
            else this.#publish(watch, { status: "error", error });
          }
        });
      } finally { clearTimeout(timer); controller.abort(); }
      try { await pause(Math.min(this.#reconnect * 2 ** Math.min(failures++, 5), 30_000), membership); } catch { return; }
    }
  }

  /** Object/null updates apply immediately; promises infer loading until resolved. */
  setAuth(state: ClientAuthInput): void | Promise<void> {
    if (this.#disposed) throw new DatabaseError({ code: "disposed", message: "Function runtime disposed" });
    if (!isAuthPromise(state)) {
      const validated = authState(state);
      this.#applyAuth(validated, ++this.#authUpdate);
      return;
    }
    const update = ++this.#authUpdate;
    this.#auth = null; this.#authPending = true; this.#authFailure = undefined;
    clearTimeout(this.#authTimer); this.#authTimer = undefined;
    this.#retire(new DatabaseError({ code: "context_changed", message: "Function auth scope is being resolved" }));
    const current = () => !this.#disposed && update === this.#authUpdate;
    const resolution = Promise.resolve(state).then(value => {
      if (current()) this.#applyAuth(authState(value), update);
    }).catch(error => {
      if (!current()) return;
      this.#authPending = false;
      this.#authFailure = new DatabaseError({ code: "authentication_failed", message: "Authentication could not be resolved" });
      this.#retire(this.#authFailure);
      throw error;
    });
    // Auth listeners may ignore the completion promise; awaiting it still reports
    // the provider/validation failure. Calls remain blocked until a later update.
    void resolution.catch(() => {});
    return resolution;
  }
  #applyAuth(validated: ClientAuthState, update: number): void {
    this.#expireAuth();
    if (this.#disposed || update !== this.#authUpdate) return;
    const next = validated !== null && validated.expiresAt !== undefined && validated.expiresAt <= Date.now() ? null : validated;
    const changed = this.#authPending || !!this.#authFailure || !sameAuth(this.#auth, next);
    this.#auth = next;
    this.#authPending = false; this.#authFailure = undefined;
    // Refreshing/removing only the deadline preserves a still-valid cache scope.
    if (changed) this.#retire(new DatabaseError({ code: "context_changed", message: "Function auth scope changed" }));
    if (update === this.#authUpdate) this.#scheduleAuthExpiry();
  }
  async dispose(): Promise<void> {
    if (!this.#disposed) {
      this.#disposed = true;
      clearTimeout(this.#authTimer); this.#authTimer = undefined;
      this.#retire(new DatabaseError({ code: "disposed", message: "Function runtime disposed" }));
    }
    await Promise.allSettled([...this.#calls]);
  }
}
