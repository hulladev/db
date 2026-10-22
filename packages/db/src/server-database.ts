import { AsyncLocalStorage } from "node:async_hooks";
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { resolveNativeExecutable } from "./native.ts";
import { defineConfig, resolveLimits, type InvocationLimits, type ResolvedLimits, type DatabaseConnection } from "./config.ts";
import { transactionDatabase, type WriteDatabase } from "./database.ts";
import { DatabaseError } from "./error.ts";
import { schemaDescriptor } from "./schema-contract.ts";
import type { SchemaDefinition } from "./schema.ts";

export interface TransactionOptions { readonly retries?: number }
export interface Database<S extends SchemaDefinition> extends WriteDatabase<S> {
  transaction<T>(handler: (tx: WriteDatabase<S>) => T | PromiseLike<T>, options?: TransactionOptions): Promise<T>;
  close(): Promise<void>;
}

const transactionScope = new AsyncLocalStorage<object>();
const retryable = new WeakSet<DatabaseError>();
const idleMilliseconds = 10_000;

const maxWaiting = 64;

const failure = (code: string, message: string) => new DatabaseError({ code, message });

/** Private process protocol. It is never exposed over the application's network. */
class Bridge {
  readonly child: ChildProcessWithoutNullStreams;
  readonly stopped = new AbortController();
  readonly exited: Promise<void>;
  #killTimer: ReturnType<typeof setTimeout> | undefined;
  #buffer = Buffer.alloc(0);
  #pending: { resolve: (value: Record<string, unknown>) => void; reject: (error: unknown) => void; timer: ReturnType<typeof setTimeout> } | undefined;
  idleSince = 0;
  idleTimer: ReturnType<typeof setTimeout> | undefined;
  dead = false;
  inTransaction = false;
  #committing = false;
  readonly limits: ResolvedLimits;
  readonly maxFrame: number;
  constructor(limits: ResolvedLimits = resolveLimits()) {
    this.limits = limits; this.maxFrame = Math.max(limits.inputBytes, limits.outputBytes, limits.readBytes) + 2 * 1024 * 1024;
    const engine = resolveNativeExecutable("bridge");
    this.child = spawn(engine, [], { stdio: ["pipe", "pipe", "pipe"], env: {} });
    this.exited = new Promise(resolve => this.child.once("close", () => {
      clearTimeout(this.#killTimer); resolve();
    }));
    // The native bridge sanitizes protocol errors. Never echo driver stderr,
    // which may contain connection or operator details.
    let diagnostics = "";
    this.child.stderr.on("data", (chunk: Buffer) => {
      diagnostics += chunk.toString("utf8");
      const lines = diagnostics.split("\n"); diagnostics = lines.pop()!.slice(-4096);
      for (const line of lines) if (line.startsWith("[hulla/db] ")) console.warn(line);
    });
    this.child.on("error", () => this.destroy(failure("engine_unavailable", "Cannot start the database engine; install/build hulladb-bridge or set HULLADB_ENGINE")));
    this.child.on("exit", () => this.destroy());
    this.child.stdin.on("error", () => this.destroy());
    this.child.stdout.on("data", (chunk: Buffer) => {
      this.#buffer = Buffer.concat([this.#buffer, chunk]);
      if (this.#buffer.length > this.maxFrame) { this.destroy(failure("execution_limit", "Database response exceeds the transport budget")); return; }
      let newline: number;
      while ((newline = this.#buffer.indexOf(10)) !== -1) {
        const line = this.#buffer.subarray(0, newline); this.#buffer = this.#buffer.subarray(newline + 1);
        const pending = this.#pending;
        if (!pending) { this.destroy(); return; }
        this.#pending = undefined; clearTimeout(pending.timer);
        try {
          const value: unknown = JSON.parse(line.toString("utf8"));
          if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error();
          pending.resolve(value as Record<string, unknown>);
        } catch { pending.reject(this.protocolFailure("Invalid database engine response")); this.destroy(); return; }
      }
    });
  }
  private protocolFailure(message: string): DatabaseError {
    return this.#committing
      ? failure("commit_outcome_unknown", "Commit confirmation was lost; the transaction may have committed")
      : failure("engine_protocol", message);
  }
  destroy(error?: DatabaseError): void {
    if (this.dead) return;
    this.dead = true;
    clearTimeout(this.idleTimer);
    const pending = this.#pending; this.#pending = undefined;
    if (pending) {
      clearTimeout(pending.timer);
      pending.reject(this.#committing ? this.protocolFailure("") : error ?? failure("engine_disconnected", "Database engine disconnected"));
    }
    this.stopped.abort(error ?? failure("engine_disconnected", "Database engine disconnected"));
    this.child.kill();
    if (this.child.exitCode === null && this.child.signalCode === null) {
      this.#killTimer = setTimeout(() => this.child.kill("SIGKILL"), 2000);
      this.#killTimer.unref();
    }
  }
  async command(frame: Record<string, unknown>, expected: string): Promise<Record<string, unknown>> {
    if (this.dead) throw failure("engine_disconnected", "Database engine is closed");
    if (this.#pending) throw failure("engine_protocol", "Concurrent engine command");
    const encoded = JSON.stringify(frame) + "\n";
    if (Buffer.byteLength(encoded) > this.maxFrame) throw failure("execution_limit", "Database request exceeds the transport budget");
    this.#committing = frame.type === "commit";
    const response = await new Promise<Record<string, unknown>>((resolve, reject) => {
      const timer = setTimeout(() => this.destroy(failure(this.#committing ? "commit_outcome_unknown" : "engine_timeout", "Database engine did not respond in time")), this.limits.executionMs + 1000);
      this.#pending = { resolve, reject, timer };
      this.child.stdin.write(encoded);
    });
    if (response.type === "error") {
      if (this.#committing && response.rolledBack !== true) {
        const error = this.protocolFailure(""); this.destroy(); throw error;
      }
      this.#committing = false;
      const info = response.error as { code?: unknown; message?: unknown } | undefined;
      const error = failure(typeof info?.code === "string" ? info.code : "database_failed", typeof info?.message === "string" ? info.message : "Database operation failed");
      this.inTransaction = false;
      if (response.rolledBack === true && error.code === "serialization_conflict") retryable.add(error);
      if (response.rolledBack !== true) this.destroy();
      throw error;
    }
    if (response.type !== expected) { const error = this.protocolFailure("Unexpected database engine response"); this.destroy(); throw error; }
    this.#committing = false;
    if (expected === "begun") this.inTransaction = true;
    if (expected === "committed" || expected === "rolled_back") this.inTransaction = false;
    return response;
  }
}

/** Internal factory called by the generated server entrypoint. Importing is inert. */
export function createDatabase<S extends SchemaDefinition>(schema: S, connection: DatabaseConnection, overrides?: InvocationLimits): Database<S> {
  const config = defineConfig({ database: { server: connection } }).database.server;
  const limits = resolveLimits(overrides);
  const descriptor = schemaDescriptor(schema);
  const idle: Bridge[] = [], sessions = new Set<Bridge>();
  const waiters: { resolve: () => void; reject: (error: unknown) => void }[] = [];
  let available = 4, closed = false;
  const assertRoot = () => {
    if (closed) throw failure("database_closed", "Database is closed");
    if (transactionScope.getStore()) throw failure("transaction_scope", "Use the supplied tx inside a transaction, not the root database");
  };
  const releasePermit = () => {
    const next = waiters.shift();
    if (next) next.resolve(); else available++;
  };
  async function acquire(): Promise<Bridge> {
    if (available > 0) available--;
    else {
      if (waiters.length >= maxWaiting) throw failure("capacity", "Database connection queue is full");
      await new Promise<void>((resolve, reject) => {
        const waiter = {
          resolve: () => { clearTimeout(timer); resolve(); },
          reject: (error: unknown) => { clearTimeout(timer); reject(error); },
        };
        const timer = setTimeout(() => {
          const index = waiters.indexOf(waiter);
          if (index !== -1) waiters.splice(index, 1);
          reject(failure("database_timeout", "Timed out waiting for a database connection"));
        }, limits.executionMs);
        waiters.push(waiter);
      });
    }
    try {
      if (closed) throw failure("database_closed", "Database is closed");
      let session: Bridge | undefined;
      while ((session = idle.pop())) {
        clearTimeout(session.idleTimer);
        if (!session.dead && Date.now() - session.idleSince < idleMilliseconds) {
          try { await session.command({ type: "ping" }, "pong"); return session; }
          catch { session.destroy(); }
        }
        session.destroy(); await session.exited; sessions.delete(session);
      }
      if (closed) throw failure("database_closed", "Database is closed");
      session = new Bridge(limits); sessions.add(session);
      try {
        await session.command({ type: "init", url: config.url, namespace: config.namespace ?? "hulla", schema: descriptor, limits, development: process.env.NODE_ENV !== "production" }, "ready");
        return session;
      } catch (error) { session.destroy(); await session.exited; sessions.delete(session); throw error; }
    } catch (error) { releasePermit(); throw error; }
  }
  async function release(session: Bridge): Promise<void> {
    if (!closed && !session.dead) {
      session.idleSince = Date.now();
      idle.push(session);
      session.idleTimer = setTimeout(() => {
        const index = idle.indexOf(session);
        if (index === -1) return;
        idle.splice(index, 1); session.destroy();
        void session.exited.then(() => sessions.delete(session));
      }, idleMilliseconds);
      session.idleTimer.unref();
    }
    else { session.destroy(); await session.exited; sessions.delete(session); }
    releasePermit();
  }
  async function execute<T>(writable: boolean, handler: (tx: WriteDatabase<S>, request: (operation: Record<string, unknown>, validate?: (result: unknown) => void) => Promise<unknown>) => T | PromiseLike<T>, scoped: boolean): Promise<T> {
    assertRoot();
    const session = await acquire();
    let active = true, pending = 0, sequence = 0;
    const token = {};
    let tail: Promise<unknown> = Promise.resolve();
    let statementError: unknown;
    let statementFailed = false;
    const deadline = setTimeout(() => session.destroy(failure("transaction_timeout", "Database transaction exceeded its execution deadline")), limits.executionMs);
    const request = (operation: Record<string, unknown>, validate?: (result: unknown) => void): Promise<unknown> => {
      if (!active || closed) return Promise.reject(failure("transaction_closed", "Transaction handle is no longer active"));
      if (scoped && transactionScope.getStore() !== token) return Promise.reject(failure("transaction_scope", "Use the transaction handle only inside its callback"));
      if (pending >= maxWaiting) {
        statementError = failure("capacity", "Too many queued operations in one transaction");
        statementFailed = true;
        return Promise.reject(statementError);
      }
      pending++;
      const result = tail.then(async () => {
        if (!active || closed) throw failure("transaction_closed", "Transaction handle is no longer active");
        const id = ++sequence;
        const response = await session.command({ type: "operation", id, operation }, "operation_result");
        if (response.id !== id) { session.destroy(); throw failure("engine_protocol", "Database operation response does not match its request"); }
        // Validation belongs to the statement's queue entry, before commit. Its
        // failure poisons the transaction even if the application catches it.
        validate?.(response.result);
        return response.result;
      }).catch(error => { statementError = error; statementFailed = true; throw error; }).finally(() => { pending--; });
      tail = result;
      void result.catch(() => {});
      return result;
    };
    try {
      await session.command({ type: "begin", mode: writable ? "write" : "read" }, "begun");
      const tx = transactionDatabase(schema, writable, request) as WriteDatabase<S>;
      const signal = session.stopped.signal;
      let stopped!: () => void;
      let result: T;
      try {
        signal.throwIfAborted();
        result = await Promise.race([
          Promise.resolve().then(() => { signal.throwIfAborted(); return transactionScope.run(token, () => handler(tx, request)); }),
          new Promise<never>((_, reject) => {
            stopped = () => reject(signal.reason);
            signal.addEventListener("abort", stopped, { once: true });
          }),
        ]);
      } finally { if (stopped) signal.removeEventListener("abort", stopped); }
      active = false;
      if (pending) { session.destroy(); throw failure("unawaited_operations", "Await all database operations before returning from a transaction"); }
      if (statementFailed) throw statementError;
      await session.command({ type: "commit" }, "committed");
      return result;
    } catch (error) {
      active = false;
      if (pending) session.destroy();
      if (session.inTransaction && !session.dead) {
        try { await session.command({ type: "rollback" }, "rolled_back"); }
        catch { session.destroy(); }
      }
      throw error;
    } finally { clearTimeout(deadline); active = false; await release(session); }
  }
  const root = transactionDatabase(schema, true, (operation, validate) =>
    execute(!["select", "paginate", "aggregate"].includes(operation.method as string), (_tx, request) => request(operation, validate), false)) as WriteDatabase<S>;
  return Object.freeze({
    ...root,
    async transaction<T>(handler: (tx: WriteDatabase<S>) => T | PromiseLike<T>, options: TransactionOptions = {}): Promise<T> {
      assertRoot();
      if (typeof handler !== "function" || typeof options !== "object" || options === null
        || Reflect.ownKeys(options).some(key => key !== "retries")
        || !Number.isInteger(options.retries ?? 0) || (options.retries ?? 0) < 0 || (options.retries ?? 0) > 100) throw new TypeError("Invalid transaction options");
      for (let attempt = 0; ; attempt++) {
        try { return await execute(true, handler, true); }
        catch (error) {
          if (!(error instanceof DatabaseError) || !retryable.has(error) || attempt >= (options.retries ?? 0)) throw error;
          await new Promise(resolve => setTimeout(resolve, Math.min(5 * 2 ** attempt, 100) * Math.random()));
        }
      }
    },
    async close(): Promise<void> {
      if (transactionScope.getStore()) throw failure("transaction_scope", "Close the database outside its transaction callback");
      if (closed) return;
      closed = true;
      for (const waiter of waiters.splice(0)) waiter.reject(failure("database_closed", "Database is closed"));
      const exiting = [...sessions].map(session => {
        session.destroy(failure("database_closed", "Database is closed"));
        return session.exited;
      });
      idle.length = 0; sessions.clear();
      await Promise.all(exiting);
    },
  });
}


/** Explicit CLI provisioning. Importing a server database never calls this. */
export async function initializeDatabase(schema: SchemaDefinition, connection: DatabaseConnection): Promise<void> {
  const config = defineConfig({ database: { server: connection } }).database.server;
  const session = new Bridge();
  try {
    await session.command({ type: "init", url: config.url, namespace: config.namespace ?? "hulla", schema: schemaDescriptor(schema), initialize: true }, "ready");
  } finally { session.destroy(); await session.exited; }
}
