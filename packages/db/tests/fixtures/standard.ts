import type { StandardSchema } from "../../src/validation.ts";

/** Test fixtures implement the portable contract without adding an application validator dependency. */
export function standard<I, O = I>(parse: (value: unknown) => O | Promise<O>): StandardSchema<I, O> {
  return {
    "~standard": {
      version: 1, vendor: "hulla-test",
      async validate(value) {
        try { return { value: await parse(value) }; }
        catch (error) { return { issues: [{ message: error instanceof Error ? error.message : "Invalid value" }] }; }
      },
    },
  };
}

type Parsers = Readonly<Record<string, { parse(value: unknown): unknown }>>;
export function objectInput<V extends Parsers>(parsers: V): StandardSchema<{ [K in keyof V]: ReturnType<V[K]["parse"]> }> {
  return standard(value => {
    if (typeof value !== "object" || value === null || Array.isArray(value) || ![Object.prototype, null].includes(Object.getPrototypeOf(value))) throw new TypeError("Function input must be a plain object");
    if (Object.keys(value).some(key => !Object.hasOwn(parsers, key))) throw new TypeError("Unknown input field");
    const input = value as Record<string, unknown>;
    const result: Record<string, unknown> = Object.create(null);
    for (const [key, parser] of Object.entries(parsers)) result[key] = parser.parse(Object.hasOwn(input, key) ? input[key] : undefined);
    return result as { [K in keyof V]: ReturnType<V[K]["parse"]> };
  });
}
