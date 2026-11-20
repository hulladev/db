import { HttpRuntime, type HttpRuntimeOptions } from "./http.ts";
import { createRuntimeScope, type RuntimeScope } from "./runtime.ts";
import { bindingInfo } from "./callable.ts";

export interface BrowserOptions extends Pick<HttpRuntimeOptions, "auth" | "headers" | "credentials" | "fetch" | "createRequestId" | "operationTimeoutMs" | "queryTimeoutMs" | "streamIdleTimeoutMs" | "confirmationTimeoutMs"> {
  readonly url: string;
}

/** Generated settings are public data; the server config is never imported here. */
export function createBrowserRuntime(contracts: HttpRuntimeOptions["contracts"], options: BrowserOptions,
  budgets?: Pick<HttpRuntimeOptions, "maxResponseBytes">) {
  const runtime = createRuntimeScope();
  let adapter: HttpRuntime | undefined;
  let headers = prepareHeaders(options.headers);
  const detach = runtime.configure(() => {
    if (!adapter) {
      const url = options.url.startsWith("/") ? new URL(options.url, globalThis.location?.origin).href : options.url;
      adapter = new HttpRuntime({ ...options, ...budgets, baseUrl: url, contracts,
        headers: () => typeof headers === "function" ? headers() : headers ?? {} });
    }
    return adapter;
  });
  let disposal: Promise<void> | undefined;
  return {
    runtime,
    setAuth: runtime.setAuth.bind(runtime),
    /** Transport credentials are independent of the cache identity passed to setAuth. */
    setHeaders(value: BrowserOptions["headers"]) { headers = prepareHeaders(value); },
    dispose() { return disposal ??= (detach(), adapter?.dispose() ?? Promise.resolve()); },
  };
}

function prepareHeaders(value: BrowserOptions["headers"]) {
  return typeof value === "function" ? value : new Headers(value);
}

/** Binds the generated catalogue once, preserving reexports of the same callable. */
export function bindClientFunctions<T extends object>(runtime: RuntimeScope, references: T): T {
  const bindings = new Map<unknown, unknown>();
  function bind(reference: unknown): unknown {
    if (bindings.has(reference)) return bindings.get(reference);
    const callable = typeof reference === 'function'
      ? bindingInfo(reference).rebind(runtime)
      : Object.freeze(Object.fromEntries(Object.entries(reference as object).map(([name, value]) => [name, bind(value)])));
    bindings.set(reference, callable);
    return callable;
  }
  return bind(references) as T;
}
