import { openDatabase, sourceRuntime } from "@hulla/db/internal/connect";
import { databaseConfiguration, type DatabaseConfiguration } from "./database-configuration.ts";
import { replicaSources, functionDatabases, type DatabaseIdentity } from "./database-metadata.ts";
import { inheritClientStorage } from "./client-storage.ts";
import { executeDefinition, isFunctionDefinition, type FunctionDefinition } from "./declarations.ts";
import type { SchemaDefinition } from "./schema.ts";
import type { ConnectedDefinition, DatabaseEngine } from "./engine.ts";
import type { FunctionSource } from "./function-source.ts";
import type { FunctionSignature, FunctionRuntime } from "./contracts.ts";
import { bindingInfo, queryBinding, mutationBinding, type QueryCallable, type MutationCallable } from "./callable.ts";
import { createRuntimeScope, type RuntimeScope } from "./runtime.ts";
import { deferredFunctions } from "./deferred.ts";
import { DatabaseError } from "./error.ts";
import { executeStorageOperation, observeStorageChanges } from "./storage-execution.ts";
import { transactionDatabase } from "./database.ts";
import { captureInput } from "./function-input.ts";

export type DatabaseState =
  | { readonly status: "initializing" | "ready" | "closed" }
  | { readonly status: "error"; readonly error: unknown };
export type FunctionBindings<T> = T extends FunctionSignature & { readonly kind: "query" } ? QueryCallable<T>
  : T extends FunctionSignature & { readonly kind: "mutation" } ? MutationCallable<T>
  : T extends object ? { readonly [K in keyof T as T[K] extends object ? K : never]: FunctionBindings<T[K]> } : never;
export type SourceFunctions<F> = F extends () => PromiseLike<infer M> ? FunctionBindings<M> : {};
export type BoundFunctions<T> = T extends FunctionSignature ? FunctionBindings<T>
  : T extends (...args: never[]) => unknown ? T
  : T extends object ? { readonly [K in keyof T]: BoundFunctions<T[K]> } : never;
declare const instanceType: unique symbol;
export type Database<S extends SchemaDefinition = SchemaDefinition, F extends FunctionSource | undefined = undefined> = ConnectedDefinition<S> & {
  readonly [instanceType]: true;
  readonly schema: S;
  readonly state: DatabaseState;
  readonly functions: SourceFunctions<F>;
  ready(): Promise<void>;
  subscribe(listener: (state: DatabaseState) => void): () => void;
  bind<T extends object>(reference: T): BoundFunctions<T>;
};
const instances = new WeakMap<object, DatabaseConfiguration>();
function namespaceObject(value: object): boolean {
  return [Object.prototype, null].includes(Object.getPrototypeOf(value)) || Object.prototype.toString.call(value) === '[object Module]';
}
const factories = new WeakMap<Function, DatabaseEngine>();
export function registerDatabaseFactory(factory: Function, engine: DatabaseEngine): void { factories.set(factory, engine); }
export function factoryEngine(factory: unknown): DatabaseEngine {
  const engine = typeof factory === "function" ? factories.get(factory) : undefined;
  if (!engine) throw new TypeError("Expected a registered database adapter factory");
  return engine;
}
export function instanceConfiguration(value: unknown): DatabaseConfiguration | undefined {
  return typeof value === "object" && value !== null ? instances.get(value) : undefined;
}

/** Common instance implementation used by optional storage adapters. */
export function createDatabase<S extends SchemaDefinition, F extends FunctionSource | undefined>(
  engine: DatabaseEngine, options: { readonly schema: S; readonly functions?: F } & object, initialize = false,
): Database<S, F> {
  const configuration = databaseConfiguration(engine, options);
  return createInstance<S, F>(configuration, initialize);
}

function createInstance<S extends SchemaDefinition, F extends FunctionSource | undefined>(configuration: DatabaseConfiguration<S>, initialize: boolean): Database<S, F> {
  const { schema, functions, identity } = configuration;
  let state: DatabaseState = Object.freeze({ status: "initializing" });
  let closed = false;
  let local: ConnectedDefinition<S> | undefined;
  let catalogue: Record<string, unknown> | undefined;
  let result!: Record<string, unknown>;
  const listeners = new Set<(state: DatabaseState) => void>();
  const invalidations = new Set<() => void>();
  const scopes = new Map<DatabaseIdentity, RuntimeScope>();
  const requiredSources = new Set(Object.values(schema.tables).flatMap(table => {
    const source = replicaSources.get(table)?.database;
    return source ? [source] : [];
  }));
  type SourceLifetime = { epoch: object; controller: AbortController; remoteSignal?: AbortSignal; stop?: () => void };
  const epochs = new Map<DatabaseIdentity, SourceLifetime>();
  const bound = new WeakMap<object, unknown>();
  const definitions = new Map<string, FunctionDefinition<"query" | "mutation", unknown, unknown, unknown>>();
  const localNames = new WeakMap<object, string>();
  const localBindings = new WeakMap<object, unknown>();
  let localReferences = 0;
  const managed = new Set<Promise<unknown>>();
  let functionTail: Promise<unknown> = Promise.resolve();
  let publicTransactions = 0;
  const lifetime = new AbortController();
  let localEpoch = {}, localView = new AbortController();
  let stopChanges: (() => void) | undefined;
  const assertOpen = () => { if (closed) throw new DatabaseError({ code: "disposed", message: "The database instance is closed" }); };
  const publish = (next: DatabaseState) => {
    state = Object.freeze(next);
    for (const listener of [...listeners]) { try { listener(state); } catch { /* Observers do not control lifetime. */ } }
  };
  const changed = () => { for (const listener of [...invalidations]) listener(); };
  const runtime: FunctionRuntime = {
    get identityEpoch() { return localEpoch; },
    get identitySignal() { return localView.signal; },
    async call(operation, options) {
      assertOpen();
      operation = { ...operation, input: captureInput(operation.input) };
      await opening;
      assertOpen();
      options.signal?.throwIfAborted();
      if (options.offline) throw new DatabaseError({ code: "offline_unavailable", message: "Local functions execute directly; offline queuing is for remote server mutations" });
      const definition = definitions.get(operation.name);
      if (!definition || definition.kind !== operation.kind) throw new TypeError("This function is not registered on the database instance");
      if (managed.size >= 64) throw new DatabaseError({ code: 'capacity', message: 'The local function queue is full' });
      const execute = () => {
        assertOpen();
        return local!.transaction(async tx => {
          const db = definition.kind === "query" ? Object.freeze({ select: tx.select.bind(tx) }) : tx;
          return executeDefinition(definition, { db, identity: null }, operation.input);
        });
      };
      const execution = configuration.engine.serialTransactions ? functionTail.then(execute) : execute();
      if (configuration.engine.serialTransactions) functionTail = execution.catch(() => {});
      managed.add(execution);
      let result: unknown;
      try { result = await execution; } finally { managed.delete(execution); }
      assertOpen();
      if (definition.kind === "mutation") changed();
      return result;
    },
    watch(operation, observer, options) {
      assertOpen();
      operation = { ...operation, input: captureInput(operation.input) };
      let active = true, running = false, dirty = true;
      const update = async () => {
        dirty = true;
        if (running || !active) return;
        running = true;
        while (dirty && active) {
          dirty = false;
          try {
            const data = await runtime.call(operation, options);
            if (active) observer({ status: "success", data, isStale: false, isOptimistic: false });
          } catch (error) { if (active) observer({ status: "error", error }); }
        }
        running = false;
      };
      const invalidate = () => { void update(); };
      const stop = () => { active = false; invalidations.delete(invalidate); options.signal?.removeEventListener("abort", stop); lifetime.signal.removeEventListener("abort", terminate); };
      const terminate = () => { if (active) observer({ status: "error", error: lifetime.signal.reason }); stop(); };
      invalidations.add(invalidate);
      options.signal?.addEventListener("abort", stop, { once: true });
      lifetime.signal.addEventListener("abort", terminate, { once: true });
      if (options.signal?.aborted) stop(); else invalidate();
      return stop;
    },
  };
  const localScope = createRuntimeScope();
  localScope.configure(() => { assertOpen(); return runtime; });
  const buildCatalogue = (module: unknown, prefix: string[] = [], seen = new Set<object>()): Record<string, unknown> => {
    if (!module || typeof module !== "object" || Array.isArray(module)) throw new TypeError("The functions import must return a module catalogue");
    if (seen.has(module)) throw new TypeError("Circular function catalogue");
    const result: Record<string, unknown> = Object.create(null);
    for (const [name, value] of Object.entries(module)) {
      if (["__proto__", "prototype", "constructor"].includes(name) || name.includes('\0')) throw new TypeError("Invalid function export name");
      const path = [...prefix, name];
      if (isFunctionDefinition(value)) {
        if (functionDatabases.get(value) !== identity) throw new TypeError(`Function belongs to a different schema: ${path.join(".")}`);
        const key = localNames.get(value) ?? path.join('.');
        if (definitions.has(key) && definitions.get(key) !== value) throw new TypeError(`Ambiguous function namespace: ${path.join('.')}`);
        definitions.set(key, value);
        localNames.set(value, key);
        let callable = localBindings.get(value);
        if (!callable) {
          callable = value.kind === "query" ? queryBinding(localScope, key, value.hasInput) : mutationBinding(localScope, key, value.hasInput);
          localBindings.set(value, callable);
        }
        result[name] = callable;
      } else if (value && typeof value === "object" && namespaceObject(value)) {
        const nested = buildCatalogue(value, path, new Set(seen).add(module));
        if (Object.keys(nested).length) result[name] = nested;
      }
    }
    return Object.freeze(result);
  };
  const opening = (async () => {
    try {
      // Validate the selected handlers before a driver acquires storage.
      if (typeof functions === "string") throw new TypeError("Function globs require the @hulla/db/build integration, or managed-service generation");
      const selected = functions ? await functions() : {};
      assertOpen();
      catalogue = buildCatalogue(selected);
      local = await openDatabase(configuration, configuration.connection, initialize);
      if (closed) return;
      inheritClientStorage(local, result);
      stopChanges = observeStorageChanges(local, change => {
        if (closed) return;
        if (change === 'scope') {
          const previous = localView;
          localEpoch = {}; localView = new AbortController();
          previous.abort(new DatabaseError({ code: 'context_changed', message: 'A replica authorization scope changed' }));
          localScope.refresh();
          changed();
        } else changed();
      });
      publish({ status: "ready" });
      for (const scope of scopes.values()) scope.refresh();
    } catch (error) { if (!closed) publish({ status: "error", error }); throw error; }
  })();
  void opening.catch(() => {});
  const getLocal = async () => { assertOpen(); await opening; assertOpen(); return local!; };
  const deferred = deferredFunctions(async () => { await getLocal(); return catalogue!; });
  let closing: Promise<void> | undefined;
  const closeInstance = () => {
    if (publicTransactions) return Promise.reject(new DatabaseError({ code: 'transaction_scope', message: 'Close the database after its explicit transaction completes' }));
    if (!closing) {
      closed = true;
      // Install before notifying observers: close() can be called reentrantly.
      closing = opening.then(async () => { await Promise.allSettled([...managed]); await local?.close(); }, () => undefined);
      lifetime.abort(new DatabaseError({ code: 'disposed', message: 'The database instance is closed' }));
      localView.abort(lifetime.signal.reason);
      stopChanges?.();
      for (const value of epochs.values()) { value.controller.abort(lifetime.signal.reason); value.stop?.(); }
      for (const scope of scopes.values()) scope.refresh();
      publish({ status: 'closed' });
      listeners.clear();
    }
    return closing;
  };
  const operations = transactionDatabase(schema, true, async (operation, validate) => {
    const db = await getLocal();
    const value = await executeStorageOperation(db, operation, validate);
    if (["insert", "update", "delete"].includes(operation.method as string)) changed();
    return value;
  });
  result = {
    ...operations,
    schema,
    get state() { return state; },
    get functions() { return catalogue ?? deferred; },
    async ready() { await getLocal(); },
    subscribe(listener: (state: DatabaseState) => void) {
      if (typeof listener !== "function") throw new TypeError("A state subscription requires a callback");
      if (!closed) listeners.add(listener);
      try { listener(state); } catch { /* Match subsequent notifications. */ }
      return () => { listeners.delete(listener); };
    },
    bind(reference: unknown): unknown {
      assertOpen();
      if (!reference || (typeof reference !== "object" && typeof reference !== "function")) throw new TypeError("Expected generated function references");
      const cached = bound.get(reference);
      if (cached) return cached;
      if (isFunctionDefinition(reference)) {
        if (functionDatabases.get(reference) !== identity) throw new TypeError('Function belongs to a different schema');
        const existing = localBindings.get(reference);
        if (existing) return existing;
        if (catalogue) throw new TypeError('This function is not registered on the database instance');
        const name = `\0local:${localReferences++}`;
        localNames.set(reference, name);
        const callable = reference.kind === 'query' ? queryBinding(localScope, name, reference.hasInput) : mutationBinding(localScope, name, reference.hasInput);
        localBindings.set(reference, callable);
        return callable;
      }
      if (typeof reference === "function") {
        const info = bindingInfo(reference);
        const source = info.scope.databaseIdentity;
        if (!source) throw new TypeError("This reference has no declared source");
        if (!requiredSources.has(source)) throw new TypeError("The function source is not declared by this instance's replica schema");
        let scope = scopes.get(source);
        if (!scope) {
          const epoch: SourceLifetime = { epoch: {}, controller: new AbortController() };
          epochs.set(source, epoch);
          scope = createRuntimeScope(undefined, source);
          const remoteFor = (db: ConnectedDefinition<S>) => {
            const remote = sourceRuntime(db, source);
            const follow = () => {
              const signal = remote.identitySignal;
              if (closed || signal === epoch.remoteSignal) return;
              epoch.stop?.(); epoch.remoteSignal = signal;
              const retired = () => {
                if (closed || epoch.remoteSignal !== signal) return;
                const previous = epoch.controller;
                epoch.epoch = {}; epoch.controller = new AbortController();
                follow();
                previous.abort(signal.reason);
              };
              signal.addEventListener("abort", retired, { once: true });
              epoch.stop = () => signal.removeEventListener("abort", retired);
            };
            follow();
            return remote;
          };
          const gateway: FunctionRuntime = {
            get clientAuth() { return local ? remoteFor(local).clientAuth : { key: null, pending: true }; },
            async call(operation, options, onStatus) { const db = await getLocal(); return remoteFor(db).call(operation, options, onStatus); },
            watch(operation, observer, options) {
              let cancelled = false, stop: (() => void) | undefined;
              void getLocal().then(db => { if (!cancelled) stop = remoteFor(db).watch(operation, observer, options); }).catch(error => { if (!cancelled) observer({ status: "error", error }); });
              return () => { cancelled = true; stop?.(); };
            },
            registerOfflineResolver(name, resolve) {
              if (local && state.status === 'ready') remoteFor(local).registerOfflineResolver(name, resolve);
              else void getLocal().then(db => remoteFor(db).registerOfflineResolver(name, resolve)).catch(() => {});
            },
            get identityEpoch() { return epoch.epoch; },
            get identitySignal() { return epoch.controller.signal; },
          };
          // Observers waiting on startup need a new runtime identity when the
          // instance becomes ready; invocation epochs themselves stay stable.
          const initializingGateway = Object.create(gateway) as FunctionRuntime;
          scope.configure(() => { assertOpen(); return state.status === "ready" ? gateway : initializingGateway; });
          scopes.set(source, scope);
        }
        const callable = info.rebind(scope);
        bound.set(reference, callable);
        return callable;
      }
      if (!namespaceObject(reference)) throw new TypeError('Expected a namespace of generated function references');
      const namespace: Record<string, unknown> = Object.create(null);
      bound.set(reference, namespace);
      try {
        for (const [name, value] of Object.entries(reference)) namespace[name] = (result.bind as (value: unknown) => unknown)(value);
        return Object.freeze(namespace);
      } catch (error) { bound.delete(reference); throw error; }
    },
    async transaction(handler: (tx: unknown) => unknown, options?: unknown) {
      const db = await getLocal();
      assertOpen(); publicTransactions++;
      let value: unknown;
      try {
        value = await Reflect.apply(db.transaction, db, [handler, options]);
      } finally { publicTransactions--; }
      changed();
      return value;
    },
    close: () => closeInstance(),
  };
  for (const method of ["pendingMutations", "resolveConflict", "acknowledgeMutation", "flush", "forgetQuery"]) result[method] = (...args: unknown[]) => {
    if (closed) return Promise.reject(new DatabaseError({ code: 'disposed', message: 'The database instance is closed' }));
    const apply = (db: ConnectedDefinition<S>) => {
      const operation = Reflect.get(db, method);
      if (typeof operation !== "function") throw new TypeError("This database has no replica sources");
      return Reflect.apply(operation, db, args);
    };
    return local && state.status === 'ready' ? apply(local) : getLocal().then(apply);
  };
  result.setAuth = (source: DatabaseIdentity, auth: unknown) => {
    assertOpen();
    const apply = (db: ConnectedDefinition<S>) => {
      const method = Reflect.get(db, "setAuth");
      if (typeof method !== "function") throw new TypeError("This database has no replica sources");
      const update = Reflect.apply(method, db, [source, auth]);
      scopes.get(source)?.refresh();
      if (update) {
        const settled = Promise.resolve(update).finally(() => scopes.get(source)?.refresh());
        void settled.catch(() => {});
        return settled;
      }
    };
    return local && state.status === "ready" ? apply(local) : getLocal().then(apply);
  };
  Object.freeze(result);
  instances.set(result, configuration);
  return result as Database<S, F>;
}
