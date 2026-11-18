/** A validator may transform or mutate its own copy, never another stage's submitted input. */
export function captureInput(value: unknown, ancestors = new Set<object>(), depth = 0): unknown {
  if (depth > 128) throw new TypeError("Function input nesting exceeds budget");
  if (value === null || value === undefined || ["string", "number", "bigint", "boolean"].includes(typeof value)) return value;
  if (typeof value !== "object" || ancestors.has(value)) throw new TypeError("Function input must contain supported plain data");
  if (value instanceof Date) {
    const time = Date.prototype.getTime.call(value);
    if (!Number.isFinite(time)) throw new TypeError("Function input dates must be valid");
    return new Date(time);
  }
  const array = Array.isArray(value);
  if (array ? Object.getPrototypeOf(value) !== Array.prototype : ![Object.prototype, null].includes(Object.getPrototypeOf(value))) throw new TypeError("Function input must contain supported plain data");
  const result: unknown[] | Record<string, unknown> = array ? [] : Object.create(Object.getPrototypeOf(value));
  const keys = Reflect.ownKeys(value);
  if (array && keys.length !== value.length + 1) throw new TypeError("Function input arrays must be dense");
  ancestors.add(value);
  try {
    for (const key of keys) {
      if (array && key === "length") continue;
      const property = Object.getOwnPropertyDescriptor(value, key)!;
      if (typeof key !== "string" || !property.enumerable || !Object.hasOwn(property, "value")) throw new TypeError("Function input properties must be ordinary values");
      if (array && (!/^(0|[1-9][0-9]*)$/.test(key) || Number(key) >= value.length)) throw new TypeError("Function input arrays must be dense");
      Object.defineProperty(result, key, { value: captureInput(property.value, ancestors, depth + 1), enumerable: true, writable: true, configurable: true });
    }
    return result;
  } finally { ancestors.delete(value); }
}
