/** JSON values have no implicit conversions or absent array/object values. */
export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
export type JsonShape =
  | { readonly kind: "any" | "string" | "boolean" | "number" }
  | { readonly kind: "nullable"; readonly inner: JsonShape }
  | { readonly kind: "array"; readonly item: JsonShape }
  | { readonly kind: "object"; readonly fields: Readonly<Record<string, { readonly shape: JsonShape; readonly optional: boolean }>> };

/** Validate and copy before storage; never invoke accessors or toJSON. */
export function validateJson(value: unknown, shape: JsonShape = { kind: "any" }, path = "$"): JsonValue {
  let nodes = 0;
  const ancestors = new Set<object>();
  function visit(value: unknown, shape: JsonShape, path: string, depth: number): JsonValue {
    const fail = (message: string): never => { throw new TypeError(`${path}: ${message}`); };
    if (++nodes > 100000 || depth > 64) fail("JSON value exceeds budget");
    if (shape.kind === "nullable") return value === null ? null : visit(value, shape.inner, path, depth + 1);
    if (value === null) { if (shape.kind !== "any") fail("Expected " + shape.kind); return null; }
    if (typeof value === "string") {
      if (shape.kind !== "any" && shape.kind !== "string") fail("Expected " + shape.kind);
      if (/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/u.test(value)) fail("Invalid Unicode");
      return value;
    }
    if (typeof value === "boolean") { if (shape.kind !== "any" && shape.kind !== "boolean") fail("Expected " + shape.kind); return value; }
    if (typeof value === "number") { if (!Number.isFinite(value) || (shape.kind !== "any" && shape.kind !== "number")) fail("Expected finite JSON number"); return value; }
    if (typeof value !== "object") return fail("Expected JSON value");
    if (ancestors.has(value)) fail("Circular JSON value");
    ancestors.add(value);
    try {
      if (Object.getOwnPropertySymbols(value).length) fail("Symbol properties are not JSON");
      if (Array.isArray(value)) {
        if (shape.kind !== "any" && shape.kind !== "array") fail("Expected " + shape.kind);
        if (Object.getOwnPropertyNames(value).length !== value.length + 1) fail("Invalid JSON array properties");
        return Array.from({ length: value.length }, (_, i) => {
          const descriptor = Object.getOwnPropertyDescriptor(value, String(i));
          if (!descriptor || !("value" in descriptor)) fail("Sparse arrays and accessors are not JSON");
          return visit(descriptor!.value, shape.kind === "array" ? shape.item : { kind: "any" }, `${path}[${i}]`, depth + 1);
        });
      }
      if (![Object.prototype, null].includes(Object.getPrototypeOf(value))) fail("Expected plain JSON object");
      if (shape.kind !== "any" && shape.kind !== "object") fail("Expected " + shape.kind);
      const result: Record<string, JsonValue> = Object.create(null);
      for (const key of Object.getOwnPropertyNames(value)) {
        const descriptor = Object.getOwnPropertyDescriptor(value, key)!;
        if (!descriptor.enumerable || !("value" in descriptor)) fail(`Non-JSON property ${key}`);
        if (shape.kind === "object" && !Object.hasOwn(shape.fields, key)) throw new TypeError(`${path}.${key}: Unknown property`);
        if (/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/u.test(key)) fail("Invalid Unicode property");
        result[key] = visit(descriptor.value, shape.kind === "object" ? shape.fields[key]!.shape : { kind: "any" }, `${path}.${key}`, depth + 1);
      }
      if (shape.kind === "object") for (const [key, field] of Object.entries(shape.fields)) if (!field.optional && !Object.hasOwn(result, key)) throw new TypeError(`${path}.${key}: Missing property`);
      return result;
    } finally { ancestors.delete(value); }
  }
  return visit(value, shape, path, 0);
}
