/** Database field parsing contract; function declarations use StandardSchema instead. */
export interface Validator<T> { parse(value: unknown): T }

/** Structurally compatible subset of Standard Schema v1 (standardschema.dev).
 * Applications supply their own validator library; the SDK has no validator dependency. */
export interface StandardSchema<I = unknown, O = I> {
  readonly "~standard": {
    readonly version: 1;
    readonly vendor: string;
    readonly types?: { readonly input: I; readonly output: O } | undefined;
    readonly validate: (value: unknown, options?: { readonly libraryOptions?: Record<string, unknown> | undefined } | undefined) =>
      | { readonly value: O; readonly issues?: undefined }
      | { readonly issues: readonly { readonly message: string; readonly path?: readonly (PropertyKey | { readonly key: PropertyKey })[] | undefined }[] }
      | Promise<
        | { readonly value: O; readonly issues?: undefined }
        | { readonly issues: readonly { readonly message: string; readonly path?: readonly (PropertyKey | { readonly key: PropertyKey })[] | undefined }[] }
      >;
  };
}
export type SchemaInput<S extends StandardSchema> = NonNullable<S["~standard"]["types"]>["input"];
export type SchemaOutput<S extends StandardSchema> = NonNullable<S["~standard"]["types"]>["output"];

export function assertStandardSchema(value: unknown): asserts value is StandardSchema {
  if ((typeof value !== "object" && typeof value !== "function") || value === null) throw new TypeError("Expected a Standard Schema validator");
  const properties = (value as StandardSchema)["~standard"];
  if (!properties || properties.version !== 1 || typeof properties.vendor !== "string" || typeof properties.validate !== "function") {
    throw new TypeError("Expected a Standard Schema v1 validator");
  }
}

export async function validateSchema(schema: StandardSchema, value: unknown, label: "Input" | "Output"): Promise<unknown> {
  const result = await schema["~standard"].validate(value);
  if (typeof result !== "object" || result === null) throw new TypeError("Invalid Standard Schema validation result");
  if (result.issues !== undefined) {
    if (!Array.isArray(result.issues) || result.issues.some(issue => typeof issue?.message !== "string")) throw new TypeError("Invalid Standard Schema issues");
    throw new TypeError(result.issues.map(issue => issue.message).join("; ") || `${label} validation failed`);
  }
  if (!Object.hasOwn(result, "value")) throw new TypeError("Invalid Standard Schema validation result");
  return result.value;
}
