import type { FunctionRuntime } from "./contracts.ts";
import type { ClientAuthInput } from "./auth.ts";
import type { OfflineResolver } from "./offline.ts";
import type { DatabaseIdentity } from "./database-metadata.ts";
import type { FunctionContracts } from "./client-storage.ts";
import { registerContracts } from "./client-registry.ts";

/** One generated application module owns one scope; no global current user. */
export class RuntimeScope {
  #resolver: (() => FunctionRuntime) | undefined;
  #listeners = new Set<() => void>();
  #offline = new Map<string, OfflineResolver<unknown>>();
  readonly #database: DatabaseIdentity | undefined;

  constructor(contracts?: FunctionContracts, database?: DatabaseIdentity, fingerprint?: string) {
    this.#database = database;
    if (database && contracts) registerContracts(database, contracts, fingerprint ?? "");
  }
  /** Internal binding ownership; source identities are never inferred from matching schemas. */
  get databaseIdentity(): DatabaseIdentity | undefined { return this.#database; }

  /** Internal callable registration. Keep one explicit policy per durable operation name. */
  registerOfflineResolver(name: string, resolver: OfflineResolver<unknown>): void {
    // An unbound reference may be decorated differently by independent apps.
    // Its immutable callable carries the policy until an instance binds it.
    if (this.#database && !this.#resolver) return;
    const previous = this.#offline.get(name);
    if (previous && previous !== resolver) throw new TypeError(`An offline resolver is already registered for ${name}; define the offline callable once`);
    this.#offline.set(name, resolver);
    this.#resolver?.().registerOfflineResolver?.(name, resolver);
  }

  /** Local lifecycle notifications used by UI bindings, not authentication proof. */
  subscribe(listener: () => void): () => void {
    this.#listeners.add(listener);
    return () => { this.#listeners.delete(listener); };
  }

  #notify(): void {
    for (const listener of [...this.#listeners]) { try { listener(); } catch { /* Observers do not control instance lifetime. */ } }
  }
  /** Instance lifecycle/auth changes invalidate only this instance's bindings. */
  refresh(): void { this.#notify(); }

  /** Install once in bootstrap. Server resolvers must use current-request scope. */
  configure(resolve: () => FunctionRuntime): () => void {
    if (this.#resolver) throw new Error("The function runtime scope is already configured");
    this.#resolver = resolve;
    this.#notify();
    return () => {
      if (this.#resolver === resolve) { this.#resolver = undefined; this.#notify(); }
    };
  }

  current(): FunctionRuntime {
    if (!this.#resolver) throw new Error(this.#database
      ? "Bind this function reference to an application database instance before calling it"
      : "No function runtime is configured for this application");
    const runtime = this.#resolver();
    if (runtime.registerOfflineResolver) for (const [name, resolver] of this.#offline) runtime.registerOfflineResolver(name, resolver);
    return runtime;
  }

  setAuth(state: ClientAuthInput): void | Promise<void> {
    const runtime = this.current();
    if (!runtime.setAuth) throw new Error("The configured runtime does not support client auth updates");
    const result = runtime.setAuth(state);
    this.#notify();
    if (result) {
      const resolution = Promise.resolve(result).finally(() => this.#notify());
      void resolution.catch(() => {});
      return resolution;
    }
  }
}

export function createRuntimeScope(contracts?: FunctionContracts, database?: DatabaseIdentity, fingerprint?: string): RuntimeScope {
  return new RuntimeScope(contracts, database, fingerprint);
}
