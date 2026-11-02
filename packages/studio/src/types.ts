export interface StudioField {
  readonly name: string;
  readonly scalar: string;
  readonly nullable: boolean;
  readonly hasDefault: boolean;
  readonly default?: unknown;
  readonly reference?: string;
  readonly onDelete?: string;
  readonly constraints?: unknown;
  readonly precision?: number;
  readonly jsonShape?: unknown;
}
export interface StudioTable {
  readonly name: string;
  readonly primaryKey: string;
  readonly keyGeneration: unknown;
  readonly fields: readonly StudioField[];
  readonly indexes: unknown;
  readonly unique: unknown;
  readonly checks: unknown;
}
export interface StudioSchema {
  readonly namespace: string;
  readonly readOnly: boolean;
  readonly tables: readonly StudioTable[];
}
/** Values use database scalar representations: int64 is a decimal string. */
export interface StudioRow { readonly values: Record<string, unknown>; readonly snapshot: string }
export interface StudioPage { readonly rows: readonly StudioRow[]; readonly cursor: string | null }
export interface StudioDeleteResult { readonly deleted: number }
