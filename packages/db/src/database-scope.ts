import { tableBindings, type DatabaseIdentity } from "./database-metadata.ts";
import { inheritClientStorage } from "./client-storage.ts";
import { inheritStorageExecution } from "./storage-execution.ts";

const owners = new WeakMap<object, { readonly owner: DatabaseIdentity; readonly scoped: object }>();

/** Carry the authoring database identity across engine handles and transaction callbacks. */
export function scopeDatabase<D extends object>(database: D, owner: DatabaseIdentity): D {
  const previous = owners.get(database);
  if (previous && previous.owner !== owner) throw new TypeError("Database handle belongs to a different database definition");
  if (previous) return previous.scoped as D;
  const methods = database as Record<string, unknown>;
  const scoped: Record<string, unknown> = { ...methods };
  for (const name of ["select", "insert", "update", "delete"]) {
    const method = methods[name];
    if (typeof method !== "function") continue;
    scoped[name] = (input: unknown, ...args: unknown[]) => {
      const source = typeof input === "object" && input !== null ? tableBindings.get(input) : undefined;
      if (source && source.database !== owner) throw new TypeError("Table belongs to a different database definition");
      return Reflect.apply(method, database, [input, ...args]);
    };
  }
  if (typeof methods.transaction === "function") {
    const transaction = methods.transaction;
    scoped.transaction = (handler: (tx: object) => unknown, ...options: unknown[]) =>
      Reflect.apply(transaction, database, [(tx: object) => handler(scopeDatabase(tx, owner)), ...options]);
  }
  if (typeof methods.close === "function") {
    const close = methods.close;
    scoped.close = (...args: unknown[]) => Reflect.apply(close, database, args);
  }
  const result = Object.freeze(scoped);
  const binding = { owner, scoped: result };
  owners.set(result, binding);
  owners.set(database, binding);
  inheritClientStorage(database, result);
  inheritStorageExecution(database, result);
  return result as D;
}
