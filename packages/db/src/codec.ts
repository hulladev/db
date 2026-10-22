import { validateJson } from "./json-value.ts";
/** Portable value shapes compiled from function signatures. No handlers ship. */
export type ValueShape =
  | { readonly kind: "string" | "number" | "boolean" | "bigint" | "date" | "null" | "undefined" | "json" }
  | { readonly kind: "literal"; readonly value: string | number | boolean }
  | { readonly kind: "array"; readonly item: ValueShape }
  | { readonly kind: "tuple"; readonly items: readonly ValueShape[] }
  | { readonly kind: "union"; readonly choices: readonly ValueShape[] }
  | { readonly kind: "object"; readonly fields: Readonly<Record<string, { readonly shape: ValueShape; readonly optional: boolean }>> };

export interface ValueCodec {
  parse(value: unknown): unknown;
  encode(value: unknown): unknown;
  decode(value: unknown): unknown;
}

function plain(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    && [null, Object.prototype].includes(Object.getPrototypeOf(value));
}
function same(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (a instanceof Date && b instanceof Date) return Date.prototype.getTime.call(a) === Date.prototype.getTime.call(b);
  if (Array.isArray(a) && Array.isArray(b)) return a.length === b.length && a.every((value, i) => same(value, b[i]));
  if (plain(a) && plain(b)) return Object.keys(a).length === Object.keys(b).length && Object.keys(a).every(key => Object.hasOwn(b, key) && same(a[key], b[key]));
  return false;
}
function dense(value: readonly unknown[]): boolean {
  for (let i = 0; i < value.length; i++) if (!Object.hasOwn(value, i)) return false;
  return true;
}
function convert(shape: ValueShape, value: unknown, mode: "parse" | "encode" | "decode", path: string, depth = 0): unknown {
  const invalid = () => new TypeError(`Invalid ${shape.kind} at ${path}`);
  if (depth > 128) throw new TypeError("Value nesting exceeds budget");
  switch (shape.kind) {
    case "json": return validateJson(value, undefined, path);
    case "string": if (typeof value !== "string" || /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/u.test(value)) throw invalid(); return value;
    case "number": if (typeof value !== "number" || !Number.isFinite(value)) throw invalid(); return value;
    case "boolean": if (typeof value !== "boolean") throw invalid(); return value;
    case "null": if (value !== null) throw invalid(); return null;
    case "undefined": if (value !== undefined && !(mode === "decode" && value === null)) throw invalid(); return mode === "encode" ? null : undefined;
    case "bigint": {
      if (mode === "decode") {
        if (typeof value !== "string" || !/^(0|-?[1-9][0-9]*)$/.test(value)) throw invalid();
        value = BigInt(value);
      }
      if (typeof value !== "bigint") throw invalid();
      return mode === "encode" ? value.toString() : value;
    }
    case "date": {
      let milliseconds: unknown;
      if (mode === "decode") {
        if (!plain(value) || Reflect.ownKeys(value).length !== 1 || !Object.hasOwn(value, "$date")) throw invalid();
        milliseconds = value.$date;
      } else {
        if (!(value instanceof Date) || Object.getPrototypeOf(value) !== Date.prototype || Reflect.ownKeys(value).length) throw invalid();
        milliseconds = Date.prototype.getTime.call(value);
      }
      if (typeof milliseconds !== "number" || !Number.isSafeInteger(milliseconds) || Math.abs(milliseconds) > 8_640_000_000_000_000) throw invalid();
      return mode === "encode" ? { $date: milliseconds } : new Date(milliseconds);
    }
    case "literal": if (value !== shape.value) throw invalid(); return value;
    case "union": {
      const matches: unknown[] = [];
      for (const choice of shape.choices) {
        try { matches.push(convert(choice, value, mode, path, depth + 1)); } catch (error) { if (!(error instanceof TypeError)) throw error; }
      }
      if (!matches.length) throw invalid();
      if (mode === "decode" && matches.some(match => !same(match, matches[0]))) throw new TypeError(`Ambiguous union wire value at ${path}`);
      if (mode === "encode") convert(shape, matches[0], "decode", path, depth + 1);
      return matches[0];
    }
    case "array":
      if (!Array.isArray(value) || !dense(value)) throw invalid();
      return value.map((item, i) => convert(shape.item, item, mode, `${path}[${i}]`, depth + 1));
    case "tuple":
      if (!Array.isArray(value) || value.length !== shape.items.length || !dense(value)) throw invalid();
      const tuple = value;
      return shape.items.map((item, i) => convert(item, tuple[i], mode, `${path}[${i}]`, depth + 1));
    case "object": {
      if (!plain(value) || Object.keys(value).some(key => !Object.hasOwn(shape.fields, key))) throw invalid();
      const result: Record<string, unknown> = Object.create(null);
      for (const [key, field] of Object.entries(shape.fields)) {
        if (!Object.hasOwn(value, key)) { if (field.optional) continue; throw new TypeError(`Missing field ${path}.${key}`); }
        result[key] = convert(field.shape, value[key], mode, `${path}.${key}`, depth + 1);
      }
      return result;
    }
  }
}

export function valueCodec(shape: ValueShape): ValueCodec {
  return Object.freeze({
    parse: (value: unknown) => convert(shape, value, "parse", "$"),
    encode: (value: unknown) => convert(shape, value, "encode", "$"),
    decode: (value: unknown) => convert(shape, value, "decode", "$"),
  });
}

export interface FunctionCodecs {
  readonly input: ValueCodec;
  readonly result: ValueCodec;
}
