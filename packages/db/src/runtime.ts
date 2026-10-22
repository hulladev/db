import type { FunctionRuntime } from "./contracts.ts";
import type { ClientAuthInput } from "./auth.ts";

/** One generated application module owns one scope; no global current user. */
export class RuntimeScope {
  #resolver: (() => FunctionRuntime) | undefined;
  #listeners = new Set<() => void>();

  /** Local lifecycle notifications used by UI bindings, not authentication proof. */
  subscribe(listener: () => void): () => void {
    this.#listeners.add(listener);
    return () => { this.#listeners.delete(listener); };
  }

  #notify(): void {
    for (const listener of [...this.#listeners]) listener();
  }

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
    if (!this.#resolver) throw new Error("No function runtime is configured for this application");
    return this.#resolver();
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

export function createRuntimeScope(): RuntimeScope {
  return new RuntimeScope();
}
