type FunctionCatalogue = Readonly<Record<string, unknown>>;

/** A static catalogue, a lazy catalogue loader, or a build-time glob. */
export type FunctionSource = FunctionCatalogue | (() => PromiseLike<FunctionCatalogue>) | string;

export function namespaceObject(value: object): boolean {
  return [Object.prototype, null].includes(Object.getPrototypeOf(value)) || Object.prototype.toString.call(value) === '[object Module]';
}

export function validateFunctionSource(value: unknown): asserts value is FunctionSource | undefined {
  if (value === undefined || typeof value === "function") return;
  if (typeof value === "object" && value !== null && namespaceObject(value)) return;
  if (typeof value === "string" && value.trim() && !value.includes("\0")) return;
  throw new TypeError("functions must be a module catalogue, lazy module import, or nonempty glob");
}
