import { HttpRuntime, type HttpRuntimeOptions } from "./http.ts";
import { createRuntimeScope } from "./runtime.ts";

export interface BrowserOptions extends Pick<HttpRuntimeOptions, "auth" | "headers" | "credentials" | "operationTimeoutMs" | "queryTimeoutMs" | "streamIdleTimeoutMs" | "confirmationTimeoutMs"> {
  readonly url: string;
}

/** Generated settings are public data; the server config is never imported here. */
export function createBrowserRuntime(contracts: HttpRuntimeOptions["contracts"], options: Omit<BrowserOptions, "auth" | "headers">,
  budgets?: Pick<HttpRuntimeOptions, "maxResponseBytes">) {
  const runtime = createRuntimeScope();
  let adapter: HttpRuntime | undefined;
  let headers: BrowserOptions["headers"];
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
    setHeaders(value: BrowserOptions["headers"]) { headers = typeof value === "function" ? value : new Headers(value); },
    dispose() { return disposal ??= (detach(), adapter?.dispose() ?? Promise.resolve()); },
  };
}
