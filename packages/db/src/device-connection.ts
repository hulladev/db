import type { DatabaseIdentity } from "./database-metadata.ts";
import type { QueuedMutation } from "./client-storage.ts";
import type { OfflineResolution } from "./offline.ts";
import type { ClientAuthInput } from "./auth.ts";
import type { QueryCallable } from "./callable.ts";
import type { FunctionSignature, Input } from "./contracts.ts";

export interface PendingMutation<Source extends DatabaseIdentity = DatabaseIdentity> extends QueuedMutation { readonly database: Source }
export interface ReplicaConnection<Source extends DatabaseIdentity = DatabaseIdentity> {
  pendingMutations(): Promise<readonly PendingMutation<Source>[]>;
  resolveConflict(mutation: Pick<PendingMutation<Source>, "database" | "id">, resolution: OfflineResolution<unknown>): Promise<void>;
  acknowledgeMutation(mutation: Pick<PendingMutation<Source>, "database" | "id">): Promise<void>;
  setAuth(database: Source, state: ClientAuthInput): void | Promise<void>;
  flush(): Promise<void>;
  forgetQuery<D extends FunctionSignature & { readonly kind: "query" }>(query: QueryCallable<D>, ...input: [Input<D>] extends [undefined] ? [input?: Input<D>] : [input: Input<D>]): Promise<void>;
}
