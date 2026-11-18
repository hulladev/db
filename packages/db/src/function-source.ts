/** An explicit catalogue, or a build-time glob relative to its declaration. */
export type FunctionSource = string | (() => PromiseLike<Readonly<Record<string, unknown>>>);

export function validateFunctionSource(value: unknown): asserts value is FunctionSource | undefined {
  if (value !== undefined && typeof value !== "function" && (typeof value !== "string" || !value.trim() || value.includes("\0")))
    throw new TypeError("functions must be a nonempty glob or a lazy module import: () => import('./functions')");
}
