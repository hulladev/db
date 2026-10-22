/** Error details are explicit JSON data, without class instances or implicit conversions. */
export type DatabaseErrorDetails =
  | null | string | number | boolean
  | readonly DatabaseErrorDetails[]
  | { readonly [key: string]: DatabaseErrorDetails };

export interface DatabaseErrorOptions {
  readonly code: string;
  readonly message: string;
  readonly details?: DatabaseErrorDetails;
}

function validString(value: string): boolean {
  return !/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/u.test(value);
}

function copyDetails(value: unknown, ancestors = new Set<object>(), depth = 0): DatabaseErrorDetails {
  const invalid = () => new TypeError("DatabaseError details must be JSON-safe data");
  if (depth > 128) throw new TypeError("DatabaseError details nesting exceeds budget");
  if (value === null || typeof value === "boolean") return value;
  if (typeof value === "string") { if (!validString(value)) throw invalid(); return value; }
  if (typeof value === "number") {
    if (!Number.isFinite(value) || (Number.isInteger(value) && !Number.isSafeInteger(value))) throw invalid();
    return value;
  }
  if (typeof value !== "object" || ancestors.has(value)) throw invalid();
  const array = Array.isArray(value);
  const prototype = Object.getPrototypeOf(value);
  if (array ? prototype !== Array.prototype : prototype !== Object.prototype && prototype !== null) throw invalid();
  ancestors.add(value);
  try {
    const result: Record<string, DatabaseErrorDetails> = {};
    const items: DatabaseErrorDetails[] = [];
    const keys = Reflect.ownKeys(value);
    if (array && keys.length !== value.length + 1) throw invalid();
    for (const key of keys) {
      if (array && key === "length") continue;
      if (typeof key !== "string" || !validString(key)) throw invalid();
      const descriptor = Object.getOwnPropertyDescriptor(value, key)!;
      if (!descriptor.enumerable || !Object.hasOwn(descriptor, "value")) throw invalid();
      if (array && (!/^(0|[1-9][0-9]*)$/.test(key) || Number(key) >= value.length)) throw invalid();
      const copied = copyDetails(descriptor.value, ancestors, depth + 1);
      if (array) items[Number(key)] = copied;
      else Object.defineProperty(result, key, { value: copied, enumerable: true });
    }
    return Object.freeze(array ? items : result);
  } finally {
    ancestors.delete(value);
  }
}

/** An intentional caller-visible failure. Unexpected exceptions stay server-local. */
export class DatabaseError extends Error {
  readonly code: string;
  readonly details?: DatabaseErrorDetails;

  constructor({ code, message, details }: DatabaseErrorOptions) {
    if (typeof code !== "string" || !code.length || !validString(code)) throw new TypeError("DatabaseError requires a nonempty code");
    if (typeof message !== "string" || !validString(message)) throw new TypeError("DatabaseError requires a message");
    super(message);
    this.name = "DatabaseError";
    this.code = code;
    if (details !== undefined) this.details = copyDetails(details);
  }

  /** Only intentionally declared fields cross a serialized error boundary. */
  toJSON(): DatabaseErrorOptions {
    return { code: this.code, message: this.message, ...(this.details === undefined ? {} : { details: this.details }) };
  }
}

const applicationErrors = new WeakSet<DatabaseError>();
/** Internal provenance: caller-chosen codes cannot control the transport. */
export function applicationError(options: DatabaseErrorOptions): DatabaseError {
  const error = new DatabaseError(options);
  applicationErrors.add(error);
  return error;
}
export function isApplicationError(error: unknown): boolean { return error instanceof DatabaseError && applicationErrors.has(error); }
