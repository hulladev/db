import { validateLimits, type InvocationLimits } from "./config.ts";
import type { FunctionKind, FunctionSignature } from "./contracts.ts";
import { assertStandardSchema, validateSchema, type SchemaInput, type SchemaOutput, type StandardSchema } from "./validation.ts";
export type { StandardSchema } from "./validation.ts";

const definitionBrand = Symbol("Hulla function definition");
type Stage = { readonly input?: StandardSchema; readonly handler: (context: unknown, input: unknown) => unknown };

export interface FunctionDefinition<K extends FunctionKind, I, O, C> extends FunctionSignature {
  readonly kind: K;
  readonly $types?: { readonly input: I; readonly output: O; readonly context: C };
  readonly hasInput: boolean;
  readonly limits?: InvocationLimits;
  readonly stages: readonly Stage[];
  readonly input?: StandardSchema;
  readonly output?: StandardSchema;
  readonly handler: (context: unknown, input: unknown) => unknown;
  readonly [definitionBrand]: true;
}

type Combine<A, B> = [A] extends [undefined] ? B : [B] extends [undefined] ? A : A & B;
type Reserved = "db" | "identity" | "__proto__" | "prototype" | "constructor";
type Collision<A, B> = Extract<keyof A, keyof B>;
type DisjointInputs<A, B> = [A] extends [undefined] ? unknown
  : A extends readonly unknown[] | ((...args: never[]) => unknown) ? { readonly combinedInputMustBeAnObject: never }
  : B extends readonly unknown[] | ((...args: never[]) => unknown) ? { readonly combinedInputMustBeAnObject: never }
  : A extends object ? B extends object ? Collision<A, B> extends never ? unknown : { readonly duplicateInputKeys: Collision<A, B> }
    : { readonly combinedInputMustBeAnObject: never }
  : { readonly combinedInputMustBeAnObject: never };
type ValidExtension<C, E> = [E] extends [void] ? unknown
  : E extends readonly unknown[] | ((...args: never[]) => unknown) ? { readonly extensionMustReturnContextProperties: never }
  : E extends object ? Extract<keyof E, keyof C | Reserved> extends never ? unknown : { readonly duplicateContextKeys: Extract<keyof E, keyof C | Reserved> }
  : { readonly extensionMustReturnContextProperties: never };
type AddedContext<E> = [E] extends [void] ? unknown : E;

export interface Declarer<K extends FunctionKind, C, I = undefined, P = undefined> {
  <O>(handler: (context: C, input: P) => O): FunctionDefinition<K, I, Awaited<O>, C>;
  <O>(options: { readonly limits?: InvocationLimits; readonly handler: (context: C, input: P) => O; readonly input?: undefined; readonly output?: undefined }): FunctionDefinition<K, I, Awaited<O>, C>;
  <S extends StandardSchema, O>(options: { readonly limits?: InvocationLimits;
    readonly input: S; readonly output?: undefined;
    readonly handler: (context: C, input: Combine<P, SchemaOutput<S>>) => O;
  } & DisjointInputs<P, SchemaOutput<S>>): FunctionDefinition<K, Combine<I, SchemaInput<S>>, Awaited<O>, C>;
  <S extends StandardSchema, O extends StandardSchema>(options: { readonly limits?: InvocationLimits;
    readonly input: S; readonly output: O;
    readonly handler: (context: C, input: Combine<P, SchemaOutput<S>>) => SchemaInput<O> | Promise<SchemaInput<O>>;
  } & DisjointInputs<P, SchemaOutput<S>>): FunctionDefinition<K, Combine<I, SchemaInput<S>>, SchemaOutput<O>, C>;
  <O extends StandardSchema>(options: { readonly limits?: InvocationLimits;
    readonly input?: undefined; readonly output: O;
    readonly handler: (context: C, input: P) => SchemaInput<O> | Promise<SchemaInput<O>>;
  }): FunctionDefinition<K, I, SchemaOutput<O>, C>;

  extend<E>(handler: (context: C, input: P) => E & ValidExtension<C, Awaited<E>>): Declarer<K, C & AddedContext<Awaited<E>>, I, P>;
  extend<E>(options: {
    readonly input?: undefined; readonly handler: (context: C, input: P) => E & ValidExtension<C, Awaited<E>>;
  }): Declarer<K, C & AddedContext<Awaited<E>>, I, P>;
  extend<S extends StandardSchema, E>(options: {
    readonly input: S;
    readonly handler: (context: C, input: Combine<P, SchemaOutput<S>>) => E & ValidExtension<C, Awaited<E>>;
  } & DisjointInputs<P, SchemaOutput<S>>): Declarer<K, C & AddedContext<Awaited<E>>, Combine<I, SchemaInput<S>>, Combine<P, SchemaOutput<S>>>;
}

function declarationOptions(value: unknown, extension: boolean): { input?: StandardSchema; output?: StandardSchema; limits?: InvocationLimits; handler: (context: unknown, input: unknown) => unknown } {
  if (typeof value === "function") return { handler: value as (context: unknown, input: unknown) => unknown };
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new TypeError("A function declaration needs an options object or handler");
  const allowed = extension ? ["input", "handler"] : ["input", "output", "handler", "limits"];
  if (Reflect.ownKeys(value).some(key => typeof key !== "string" || !allowed.includes(key))) throw new TypeError("Unknown function declaration option");
  const options = value as { input?: StandardSchema; output?: StandardSchema; limits?: InvocationLimits; handler: (context: unknown, input: unknown) => unknown };
  if (typeof options.handler !== "function") throw new TypeError("A function declaration needs a handler");
  if (options.input !== undefined) assertStandardSchema(options.input);
  if (options.output !== undefined) assertStandardSchema(options.output);
  return { ...(options.limits === undefined ? {} : { limits: validateLimits(options.limits) }), handler: options.handler, ...(options.input ? { input: options.input } : {}), ...(options.output ? { output: options.output } : {}) };
}

function declarer<K extends FunctionKind, C>(kind: K, stages: readonly Stage[] = []): Declarer<K, C> {
  const declare = (...args: unknown[]) => {
    if (args.length !== 1) throw new TypeError("A function declaration takes one options object or handler");
    const options = declarationOptions(args[0], false);
    return Object.freeze({ kind, hasInput: options.input !== undefined || stages.some(stage => stage.input !== undefined), stages, ...options, [definitionBrand]: true });
  };
  const extend = (...args: unknown[]) => {
    if (args.length !== 1) throw new TypeError("An extension takes one options object or handler");
    const { input, handler } = declarationOptions(args[0], true);
    return declarer<K, C>(kind, Object.freeze([...stages, Object.freeze({ handler, ...(input ? { input } : {}) })]));
  };
  return Object.assign(declare, { extend }) as Declarer<K, C>;
}

/** Internal constructor used by generated schema-bound authoring modules. */
export function defineFunctions<QueryContext, MutationContext>() {
  return { query: declarer<"query", QueryContext>("query"), mutation: declarer<"mutation", MutationContext>("mutation") };
}

type UntypedDefinition = FunctionDefinition<FunctionKind, unknown, unknown, unknown>;
export function isFunctionDefinition(value: unknown): value is UntypedDefinition {
  return typeof value === "object" && value !== null && (value as UntypedDefinition)[definitionBrand] === true;
}

const unsafeKeys = new Set(["__proto__", "prototype", "constructor"]);
function entries(value: unknown, label: string): readonly [string, unknown][] {
  if (typeof value !== "object" || value === null || Array.isArray(value) || ![Object.prototype, null].includes(Object.getPrototypeOf(value))) {
    throw new TypeError(`${label} must be a plain object`);
  }
  return Reflect.ownKeys(value).map(key => {
    if (typeof key !== "string" || unsafeKeys.has(key)) throw new TypeError(`Invalid ${label} key`);
    const descriptor = Object.getOwnPropertyDescriptor(value, key)!;
    if (!descriptor.enumerable || !Object.hasOwn(descriptor, "value")) throw new TypeError(`${label} properties must be ordinary values`);
    return [key, descriptor.value];
  });
}

/** A validator may transform or mutate its own copy, never another stage's submitted input. */
function copySubmitted(value: unknown, ancestors = new Set<object>(), depth = 0): unknown {
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
      Object.defineProperty(result, key, { value: copySubmitted(property.value, ancestors, depth + 1), enumerable: true, writable: true, configurable: true });
    }
    return result;
  } finally { ancestors.delete(value); }
}

/** Validate and execute stages inside the provider's snapshot/transaction; output finishes before commit. */
export async function executeDefinition(definition: UntypedDefinition, context: unknown, raw: unknown): Promise<unknown> {
  if (!definition.hasInput && raw !== undefined) throw new TypeError("This function takes no input");
  let currentContext: Record<string, unknown> = Object.create(null);
  for (const [key, value] of entries(context, "Context")) currentContext[key] = value;
  currentContext = Object.freeze(currentContext);
  const inputStages = definition.stages.filter(stage => stage.input).length + (definition.input ? 1 : 0);
  const submitted = definition.hasInput ? copySubmitted(raw) : undefined;
  let input: unknown;
  const parsedKeys = new Set<string>();
  const validateInput = async (schema?: StandardSchema) => {
    if (!schema) return;
    const parsed = await validateSchema(schema, copySubmitted(submitted), "Input");
    if (inputStages === 1) { input = parsed; return; }
    const merged: Record<string, unknown> = Object.assign(Object.create(null), input);
    for (const [key, value] of entries(parsed, "Parsed input")) {
      if (parsedKeys.has(key)) throw new TypeError(`Duplicate parsed input key: ${key}`);
      parsedKeys.add(key);
      merged[key] = value;
    }
    input = Object.freeze(merged);
  };
  for (const stage of definition.stages) {
    await validateInput(stage.input);
    const addition = await stage.handler(currentContext, input);
    if (addition === undefined) continue;
    const merged: Record<string, unknown> = Object.assign(Object.create(null), currentContext);
    for (const [key, value] of entries(addition, "Extension context")) {
      if (key === "db" || key === "identity" || Object.hasOwn(currentContext, key)) throw new TypeError(`Duplicate or reserved context key: ${key}`);
      merged[key] = value;
    }
    currentContext = Object.freeze(merged);
  }
  await validateInput(definition.input);
  const result = await definition.handler(currentContext, input);
  return definition.output ? validateSchema(definition.output, result, "Output") : result;
}
