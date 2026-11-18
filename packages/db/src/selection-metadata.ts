import type { SchemaDefinition } from "./schema.ts";
import type { Predicate } from "./filters.ts";

/** Internal provenance retained through async handlers. Mapped arrays have no provenance. */
export interface RowSelection {
  readonly schema: SchemaDefinition;
  readonly table: string;
  readonly columns: readonly string[];
  readonly filters: readonly Predicate[];
  readonly order: readonly { readonly field: string; readonly direction: "asc" | "desc" }[];
  readonly limit: number | null;
  readonly validate: () => void;
}
export const selectionResults = new WeakMap<object, RowSelection>();
