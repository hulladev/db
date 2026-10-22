/** Type-only marker identifying direct database page results. */
export declare const pageEligibility: unique symbol;
export interface Page<R> {
  readonly page: R[];
  readonly cursor: string | null;
  readonly [pageEligibility]: true;
}
export interface PageOptions { readonly limit: number; readonly cursor?: string | null }
