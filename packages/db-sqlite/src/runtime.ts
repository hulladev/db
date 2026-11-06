import { registerClientStorage, registerStorageExecution, registerStorageChanges, transactionDatabase, type WriteDatabase, type StorageExecution, type StorageChange } from "@hulla/db/internal/engine";
import type { SchemaDefinition } from "@hulla/db";
import type { SQLiteDriver } from "./driver.ts";
import { SQLiteEngine, sqliteFailure } from "./engine.ts";
import { sqliteClientStorage } from "./client-storage.ts";
type Atomic = <T>(handler: () => T | PromiseLike<T>) => Promise<T>;

export interface SQLiteOptions {
  /** Platform CSPRNG when Web Crypto is unavailable, e.g. expo-crypto.getRandomBytes. */
  readonly randomBytes?: (length: number) => Uint8Array;
  /** Callback deadline, excluding time in the connection queue. Defaults to 5000. */
  readonly transactionTimeoutMs?: number;
}
export interface SQLiteDatabase<S extends SchemaDefinition> extends WriteDatabase<S> {
  /** Runs once. Use only the supplied tx until the callback completes. */
  transaction<T>(handler: (tx: WriteDatabase<S>) => T | PromiseLike<T>): Promise<T>;
  close(): Promise<void>;
}

/** Open schema-typed local storage. The driver belongs exclusively to the returned handle. */
export async function openSQLiteDatabase<S extends SchemaDefinition>(schema: S, driver: SQLiteDriver, options: SQLiteOptions = {}, initialize = false): Promise<SQLiteDatabase<S>> {
  let engine: SQLiteEngine;
  const timeoutMs = options.transactionTimeoutMs ?? 5000;
  try {
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 60_000) throw new TypeError("transactionTimeoutMs must be an integer from 1 to 60000");
    engine = new SQLiteEngine(driver, schema, options.randomBytes);
  }
  catch (error) { await driver.close(); throw error; }
  let closed = false, userTransaction = false, queued = 0;
  let tail: Promise<unknown> = Promise.resolve();
  function enqueue<T>(handler: () => T | PromiseLike<T>, internal = false): Promise<T> {
    if (closed) return Promise.reject(sqliteFailure("database_closed", "Database is closed"));
    if (userTransaction && !internal) return Promise.reject(sqliteFailure("transaction_scope", "Use the supplied tx while a SQLite transaction is active"));
    if (queued >= 64) return Promise.reject(sqliteFailure("capacity", "SQLite connection queue is full"));
    queued++;
    const result = tail.then(async () => {
      let began = false;
      try {
        await engine.execute("BEGIN IMMEDIATE"); began = true;
        const value = await handler();
        await engine.execute("COMMIT"); began = false;
        return value;
      } catch (error) {
        if (began) {
          try { await engine.execute("ROLLBACK"); }
          catch { closed = true; await driver.close(); }
        }
        throw error;
      } finally { queued--; }
    });
    tail = result.catch(() => {});
    return result;
  }
  const atomic: Atomic = handler => enqueue(handler);
  const listeners = new Set<(change: StorageChange) => void>();
  const notify = (change: StorageChange) => { for (const listener of [...listeners]) listener(change); };
  let storage: ReturnType<typeof sqliteClientStorage>;
  try { storage = sqliteClientStorage(engine, schema, handler => enqueue(handler, true), notify); }
  catch (error) { await driver.close(); throw error; }
  try { if (initialize) await engine.initialize(); else await engine.verify(); } catch(error) { await driver.close(); throw error; }
  function guard(operation: Record<string, unknown>) {
    if (typeof operation.table === "string" && storage.isReplica(operation.table)) {
      if (["insert", "update", "delete"].includes(String(operation.method))) throw sqliteFailure("replica_read_only", "Write through a server mutation; offline calls queue that mutation explicitly");
      storage.assertReadable(operation.table);
      return true;
    }
    return false;
  }

  function transaction<T>(handler: (tx: WriteDatabase<S>) => T | PromiseLike<T>): Promise<T> {
    return atomic(async () => {
      userTransaction = true;
      let replicaEpoch: number | undefined;
      let live = true, pending = 0, failed = false;
      let failure: unknown;
      let statements: Promise<unknown> = Promise.resolve();
      let timer: ReturnType<typeof setTimeout> | undefined;
      const request = (operation: Record<string, unknown>, validate?: (result: unknown) => void) => {
        if (!live) return Promise.reject(sqliteFailure("transaction_closed", "Transaction handle is no longer active"));
        if (pending >= 64) {
          failed = true; failure = sqliteFailure("capacity", "Too many queued SQLite operations");
          return Promise.reject(failure);
        }
        pending++;
        const result = statements.then(async () => {
          if (!live) throw sqliteFailure("transaction_closed", "Transaction handle is no longer active");
          if (guard(operation)) replicaEpoch ??= storage.epoch;
          const result = await engine.operation(operation);
          validate?.(result);
          return result;
        }).catch(error => { failed = true; failure = error; throw error; }).finally(() => { pending--; });
        statements = result;
        void result.catch(() => {});
        return result;
      };
      try {
        const tx = transactionDatabase(schema, true, request) as WriteDatabase<S>;
        const result = await Promise.race([
          Promise.resolve().then(() => handler(tx)),
          new Promise<never>((_, reject) => {
            timer = setTimeout(() => reject(sqliteFailure("transaction_timeout", "SQLite transaction callback exceeded its deadline")), timeoutMs);
          }),
        ]);
        live = false;
        if (pending) throw sqliteFailure("unawaited_operations", "Await all database operations before returning from a transaction");
        if (failed) throw failure;
        if (replicaEpoch !== undefined && replicaEpoch !== storage.epoch) throw sqliteFailure("context_changed", "Replica scope changed during the transaction");
        return result;
      } finally {
        clearTimeout(timer);
        live = false;
        // An already-started asynchronous driver operation must finish before rollback.
        await statements.catch(() => {});
        userTransaction = false;
      }
    });
  }
  const execute: StorageExecution = (operation, validate) => atomic(async () => {
    const replica = guard(operation), epoch = storage.epoch;
    const result = await engine.operation(operation);
    if (replica && epoch !== storage.epoch) throw sqliteFailure("context_changed", "Replica scope changed");
    validate?.(result);
    return result;
  });
  const root = transactionDatabase(schema, true, execute) as WriteDatabase<S>;
  let closing: Promise<void> | undefined;
  const database = Object.freeze({
    ...root,
    transaction,
    close() {
      if (userTransaction) return Promise.reject(sqliteFailure("transaction_scope", "Close SQLite after the transaction completes"));
      if (!closing) { closed = true; closing = tail.then(() => driver.close()); }
      return closing;
    },
  } satisfies SQLiteDatabase<S>);
  registerClientStorage(database, storage.factory);
  registerStorageExecution(database, execute);
  registerStorageChanges(database, listener => { listeners.add(listener); return () => { listeners.delete(listener); }; });
  return database;
}
