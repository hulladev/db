/** Adapter-only transport. Application contexts expose typed operations, never this capability. */
export type StorageExecution = (operation: Record<string, unknown>, validate?: (result: unknown) => void) => Promise<unknown>;
const executions = new WeakMap<object, StorageExecution>();
export type StorageChange = 'rows' | 'scope';
type Changes = (listener: (change: StorageChange) => void) => () => void;
const changes = new WeakMap<object, Changes>();
export function registerStorageChanges(database: object, subscribe: Changes): void { changes.set(database, subscribe); }
export function observeStorageChanges(database: object, listener: (change: StorageChange) => void): () => void { return changes.get(database)?.(listener) ?? (() => {}); }
export function registerStorageExecution(database: object, execute: StorageExecution): void { executions.set(database, execute); }
export function inheritStorageExecution(from: object, to: object): void {
  const execute = executions.get(from);
  if (execute) executions.set(to, execute);
  const subscribe = changes.get(from);
  if (subscribe) changes.set(to, subscribe);
}
export function executeStorageOperation(database: object, operation: Record<string, unknown>, validate?: (result: unknown) => void): Promise<unknown> {
  const execute = executions.get(database);
  if (!execute) throw new TypeError('This storage adapter has no instance execution capability');
  return execute(operation, validate);
}
