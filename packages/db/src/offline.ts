import type { DatabaseError } from "./error.ts";

export interface OfflineConflict<I> {
  readonly requestId: string;
  readonly input: I;
  readonly error: DatabaseError;
  readonly attempt: number;
}

/** Retrying is an explicit new invocation after a definitive server rejection. */
export type OfflineResolution<I> =
  | { readonly action: "retry"; readonly input: I }
  | { readonly action: "reject" }
  | { readonly action: "defer" };

export type OfflineResolver<I> = (conflict: OfflineConflict<I>) => OfflineResolution<I> | PromiseLike<OfflineResolution<I>>;
