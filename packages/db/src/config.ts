import type { BrowserOptions } from "./browser.ts";
/** Per-invocation budgets. Encoded byte budgets exclude protocol envelopes. */
export interface InvocationLimits {
  readonly executionMs?: number;
  readonly inputBytes?: number;
  readonly outputBytes?: number;
  readonly memoryBytes?: number;
  readonly stackBytes?: number;
  readonly operations?: number;
  /** Rows actually returned by database operations, not rows scanned by SQL. */
  readonly readRows?: number;
  /** Fetched PostgreSQL binary column payloads, excluding transport framing. */
  readonly readBytes?: number;
}
export const defaultLimits = Object.freeze({ executionMs: 5000, inputBytes: 16 * 1024 * 1024,
  outputBytes: 16 * 1024 * 1024, memoryBytes: 64 * 1024 * 1024, stackBytes: 512 * 1024,
  operations: 1000, readRows: 100_000, readBytes: 64 * 1024 * 1024 });
export type ResolvedLimits = Readonly<Required<InvocationLimits>>;
const ceilings: ResolvedLimits = { executionMs: 60_000, inputBytes: 64 * 1024 * 1024,
  outputBytes: 64 * 1024 * 1024, memoryBytes: 1024 * 1024 * 1024, stackBytes: 16 * 1024 * 1024,
  operations: 1_000_000, readRows: 1_000_000, readBytes: 64 * 1024 * 1024 };
export function validateLimits(value: InvocationLimits): Readonly<InvocationLimits> {
  record(value, Object.keys(defaultLimits), "limits");
  for (const [key, setting] of Object.entries(value)) {
    const ceiling = ceilings[key as keyof ResolvedLimits];
    if (typeof setting !== "number" || !Number.isSafeInteger(setting) || setting < 1 || setting > ceiling)
      throw new TypeError(`limits.${key} must be an integer from 1 to ${ceiling}`);
  }
  return Object.freeze({ ...value });
}
export function resolveLimits(value?: InvocationLimits): ResolvedLimits {
  return Object.freeze({ ...defaultLimits, ...(value === undefined ? {} : validateLimits(value)) });
}

/** Ordinary server connection settings. Never included in generated client code. */
export interface DatabaseConnection {
  readonly url: string;
  readonly namespace?: string;
}

/** Verified provider data; never accepted as proof from a browser payload. */
export interface VerifiedIdentity {
  readonly key: string;
  readonly identity: unknown;
}
export type VerifyIdentity = (headers: Headers) => VerifiedIdentity | null | PromiseLike<VerifiedIdentity | null>;
export interface ServiceOptions {
  readonly host?: string;
  readonly port?: number;
  /** Browser application origins allowed to call the service. */
  readonly origins?: readonly string[];
}

/** Public build-time settings copied into generated browser modules. */
export type ClientConnection = Omit<BrowserOptions, "auth" | "headers">;
export function clientConnection(value: ClientConnection): Readonly<ClientConnection> {
  record(value, ["url", "credentials", "operationTimeoutMs", "queryTimeoutMs", "streamIdleTimeoutMs", "confirmationTimeoutMs"], "database.client");
  if (typeof value.url !== "string") throw new TypeError("database.client.url must be an HTTP endpoint or root-relative path");
  const relative = value.url.startsWith("/") && !value.url.startsWith("//");
  let url: URL;
  try { url = new URL(value.url, relative ? "https://client.invalid" : undefined); }
  catch { throw new TypeError("Invalid database.client.url"); }
  if (url.username || url.password || url.search || url.hash || /[\\\s]/.test(value.url)
    || !(url.protocol === "https:" || (url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname))))
    throw new TypeError("Use HTTPS or loopback HTTP without URL credentials for database.client.url");
  if (value.credentials !== undefined && !["omit", "same-origin", "include"].includes(value.credentials)) throw new TypeError("Invalid database.client.credentials");
  for (const key of ["operationTimeoutMs", "queryTimeoutMs", "streamIdleTimeoutMs", "confirmationTimeoutMs"] as const)
    if (value[key] !== undefined && (!Number.isSafeInteger(value[key]) || value[key]! < 1 || value[key]! > 2_147_483_647)) throw new TypeError(`Invalid database.client.${key}`);
  return Object.freeze({ ...value });
}
export interface DatabaseConfig {
  readonly database: { readonly server: DatabaseConnection; readonly client?: ClientConnection };
  readonly limits?: InvocationLimits;
  /** Resolved relative to this config file. Defaults to ./db/schema.ts. */
  readonly schema?: string;
  /** Optional discovery root. Defaults to the schema's directory. */
  readonly functions?: string;
  /** Return null for anonymous access; throw for invalid/expired credentials. */
  readonly auth?: VerifyIdentity;
  readonly service?: ServiceOptions;
}

export type ConfigIdentity<C> = C extends { readonly auth?: infer Verify }
  ? NonNullable<Verify> extends (...args: never[]) => infer Result
    ? [Exclude<Awaited<Result>, null>] extends [never] ? null
      : Exclude<Awaited<Result>, null> extends { readonly identity: infer Identity } ? Identity | null : null
    : null
  : null;

function record(value: unknown, keys: readonly string[], name: string): asserts value is Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)
    || ![Object.prototype, null].includes(Object.getPrototypeOf(value))
    || Reflect.ownKeys(value).some(key => typeof key !== "string" || !keys.includes(key)))
    throw new TypeError(`Invalid ${name} configuration`);
}

/** Validate configuration without opening connections, reading files, or starting a listener. */
export function defineConfig<C extends DatabaseConfig>(config: C): Readonly<C & { readonly limits?: InvocationLimits }> {
  record(config, ["database", "schema", "functions", "auth", "service", "limits"], "database");
  record(config.database, ["server", "client"], "database connection");
  record(config.database.server, ["url", "namespace"], "database.server");
  const { url, namespace } = config.database.server;
  if (typeof url !== "string" || !url.trim()) throw new TypeError("database.server.url must be a PostgreSQL connection URL");
  try {
    const parsed = new URL(url);
    if (!["postgres:", "postgresql:"].includes(parsed.protocol)) throw new Error();
  } catch { throw new TypeError("database.server.url must be a PostgreSQL connection URL"); }
  if (namespace !== undefined && (typeof namespace !== "string" || !/^[a-z_][a-z0-9_]{0,62}$/.test(namespace)))
    throw new TypeError("database.server.namespace must be a PostgreSQL identifier of at most 63 characters");
  for (const key of ["schema", "functions"] as const)
    if (config[key] !== undefined && (typeof config[key] !== "string" || !config[key].trim()))
      throw new TypeError(`${key} must be a nonempty path`);
  if (config.auth !== undefined && typeof config.auth !== "function") throw new TypeError("auth must verify credentials with a callback");
  if (config.service !== undefined) {
    record(config.service as unknown, ["host", "port", "origins"], "service");
    const { host, port, origins } = config.service;
    if (host !== undefined && (typeof host !== "string" || !host.trim() || /[\s/]/.test(host))) throw new TypeError("service.host must be a hostname or IP address");
    if (port !== undefined && (!Number.isInteger(port) || port < 0 || port > 65535)) throw new TypeError("service.port must be an integer from 0 to 65535");
    if (origins !== undefined && (!Array.isArray(origins) || origins.some(origin => {
      try { const value = new URL(origin); return typeof origin !== "string" || value.origin !== origin || !["http:", "https:"].includes(value.protocol); }
      catch { return true; }
    }))) throw new TypeError("service.origins must contain exact HTTP(S) origins");
  }
  const limits = config.limits === undefined ? undefined : validateLimits(config.limits);
  return Object.freeze({
    ...(limits === undefined ? {} : { limits }),
    database: Object.freeze({ server: Object.freeze({ url, ...(namespace === undefined ? {} : { namespace }) }),
      ...(config.database.client === undefined ? {} : { client: clientConnection(config.database.client) }) }),
    ...(config.schema === undefined ? {} : { schema: config.schema }),
    ...(config.functions === undefined ? {} : { functions: config.functions }),
    ...(config.auth === undefined ? {} : { auth: config.auth }),
    ...(config.service === undefined ? {} : { service: Object.freeze({ ...config.service,
      ...(config.service.origins === undefined ? {} : { origins: Object.freeze([...config.service.origins]) }),
    }) }),
  }) as Readonly<C & { readonly limits?: InvocationLimits }>;
}
