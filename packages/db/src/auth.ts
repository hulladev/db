/** Client cache identity only. Credentials are supplied separately by the transport. */
export type ClientAuthState = null | {
  readonly key: string;
  /** Optional client invalidation deadline, as Unix time in milliseconds. */
  readonly expiresAt?: number;
};

export type ClientAuthInput = ClientAuthState | PromiseLike<ClientAuthState>;

export function isAuthPromise(value: ClientAuthInput): value is PromiseLike<ClientAuthState> {
  return value !== null && (typeof value === "object" || typeof value === "function")
    && "then" in value && typeof value.then === "function";
}

export function authState(value: ClientAuthState): ClientAuthState {
  if (value === null) return null;
  if (typeof value !== "object" || Array.isArray(value)) throw new TypeError("Invalid client auth state");
  if (typeof value.key === "string" && value.key.length > 0
    && Object.keys(value).every(key => key === "key" || key === "expiresAt")) {
    if (value.expiresAt !== undefined && (!Number.isSafeInteger(value.expiresAt) || value.expiresAt < 0)) {
      throw new TypeError("expiresAt must be a non-negative Unix timestamp in milliseconds");
    }
    return Object.freeze({ key: value.key,
      ...(value.expiresAt === undefined ? {} : { expiresAt: value.expiresAt }) });
  }
  throw new TypeError("Auth state accepts only a cache key with optional expiresAt, or null");
}

export function sameAuth(a: ClientAuthState, b: ClientAuthState): boolean {
  return a === b || (a !== null && b !== null && a.key === b.key);
}
