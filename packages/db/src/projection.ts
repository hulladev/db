/** Inclusion-only selections. Nullish values and false support conditional fields. */
export type Projection<R> =
  | readonly (Extract<keyof R, string> | false | null | undefined)[]
  | { readonly [K in keyof R]?: boolean | null | undefined };

export type ValidProjection<R, P> = P extends readonly unknown[]
  ? unknown
  : Record<Exclude<keyof P, keyof R>, never>;

type IsUnion<T, U = T> = T extends U ? [U] extends [T] ? false : true : never;
type GuaranteedEntry<R, V> = [V] extends [keyof R]
  ? true extends IsUnion<V> ? never : V
  : never;
type RequiredKeys<R, P> = P extends readonly unknown[]
  ? number extends P["length"] ? never : { [I in keyof P]: GuaranteedEntry<R, P[I]> }[number]
  : { [K in keyof P]: [P[K]] extends [true] ? K : never }[keyof P];
type IncludedKeys<R, P> = P extends readonly unknown[]
  ? Extract<P[number], keyof R>
  : { [K in keyof P]: true extends P[K] ? K : never }[keyof P];
export type Projected<R, P> =
  Pick<R, Extract<RequiredKeys<R, P>, keyof R>>
  & Partial<Pick<R, Exclude<Extract<IncludedKeys<R, P>, keyof R>, RequiredKeys<R, P>>>>;

/** Capture the effective selection now, independently of subsequent application changes. */
export function parseProjection(fields: Readonly<Record<string, unknown>>, input: unknown): readonly string[] {
  const selected = new Set<string>();
  function field(key: unknown) {
    if (typeof key !== "string" || !Object.hasOwn(fields, key)) throw new TypeError(`Unknown projection field ${String(key)}`);
    selected.add(key);
  }
  if (Array.isArray(input)) {
    for (const key of input) {
      if (key !== false && key !== null && key !== undefined) field(key);
    }
  } else {
    if (!input || typeof input !== "object" || ![null, Object.prototype].includes(Object.getPrototypeOf(input))) throw new TypeError("Expected field selection array or inclusion object");
    for (const key of Reflect.ownKeys(input)) {
      if (typeof key !== "string" || !Object.hasOwn(fields, key)) throw new TypeError(`Unknown projection field ${String(key)}`);
      const value = (input as Record<string, unknown>)[key];
      if (value === true) field(key);
      else if (value !== false && value !== null && value !== undefined) throw new TypeError(`Invalid selection value for ${key}`);
    }
  }
  if (!selected.size) throw new TypeError("Field selection must include at least one field");
  return Object.freeze([...selected]);
}
