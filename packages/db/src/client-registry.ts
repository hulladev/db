import type { DatabaseIdentity } from "./database-metadata.ts";
import type { FunctionContracts } from "./client-storage.ts";

interface Entry {
  readonly contracts: FunctionContracts;
  readonly fingerprint: string;
}
const entries = new WeakMap<DatabaseIdentity, Entry>();
/** Contract metadata may be shared. Connections, credentials and policies never are. */
export function registerContracts(database: DatabaseIdentity, contracts: FunctionContracts, fingerprint: string): void {
  const previous = entries.get(database);
  if (previous) {
    if (previous.fingerprint !== fingerprint) throw new TypeError("Different function contracts were imported for the same source identity");
    return;
  }
  entries.set(database, { contracts, fingerprint });
}
export function sourceContracts(database: DatabaseIdentity): FunctionContracts {
  const contracts = entries.get(database)?.contracts;
  if (!contracts) throw new Error("Import this source's generated function references before using its remote operations");
  return contracts;
}
