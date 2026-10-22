import { valueCodec, type ValueShape } from "./codec.ts";
import { transactionDatabase } from "./database.ts";
import { executeDefinition, isFunctionDefinition } from "./declarations.ts";
import type { SchemaDefinition } from "./schema.ts";
import { schemaDescriptor } from "./schema-contract.ts";
import { DatabaseError } from "./error.ts";

export interface WorkerApplication {
  readonly schema: SchemaDefinition;
  readonly modules: Readonly<Record<string, Readonly<Record<string, unknown>>>>;
}

type NativeRequest = (operation: string) => Promise<string>;
type Invocation = Readonly<Record<string, unknown>>;
type Execute = (invocation: Invocation) => Promise<string>;

/** Compare JSON contracts independently of object property insertion order. */
function sameContract(left: unknown, right: unknown, depth = 0): boolean {
  if (depth > 128) return false;
  if (left === right) return true;
  if (typeof left !== "object" || left === null || typeof right !== "object" || right === null) return false;
  if (Array.isArray(left) !== Array.isArray(right)) return false;
  const leftKeys = Object.keys(left), rightKeys = Object.keys(right);
  if (leftKeys.length !== rightKeys.length) return false;
  return leftKeys.every(key => Object.hasOwn(right, key)
    && sameContract((left as Record<string, unknown>)[key], (right as Record<string, unknown>)[key], depth + 1));
}

/** Install one portable application in a fresh restricted native JavaScript realm.
 * Native code owns capabilities, transaction lifetime, and execution budgets.
 */
export function installWorker(application: WorkerApplication): void {
  const nativeRequest = (globalThis as unknown as { __hullaDb?: NativeRequest }).__hullaDb;
  if (typeof nativeRequest !== "function") throw new Error("The native database bridge is unavailable");
  const contract = schemaDescriptor(application.schema);
  const execute: Execute = async invocation => {
    let active = true, pending = 0;
    let tail: Promise<unknown> = Promise.resolve();
    const request = (operation: Record<string, unknown>, validate?: (result: unknown) => void): Promise<unknown> => {
      if (!active) return Promise.reject(new Error("Transaction context has expired"));
      pending++;
      const result = tail.then(async () => {
        if (!active) throw new Error("Transaction context has expired");
        const response: unknown = JSON.parse(await nativeRequest(JSON.stringify(operation)));
        validate?.(response);
        return response;
      }).finally(() => { pending--; });
      // Calls share one native transaction. A failed request poisons this chain
      // even when application code catches the individual operation's error.
      tail = result;
      void result.catch(() => {});
      return result;
    };
    try {
      if (!sameContract(contract, invocation.schema)) throw new Error("schema_mismatch");
      const name = invocation.name;
      if (typeof name !== "string") throw new TypeError("Invalid function name");
      const separator = name.lastIndexOf(".");
      if (separator < 1) throw new TypeError("Invalid function name");
      const moduleName = name.slice(0, separator), exportName = name.slice(separator + 1);
      const module = Object.hasOwn(application.modules, moduleName) ? application.modules[moduleName] : undefined;
      const definition = module && Object.hasOwn(module, exportName) ? module[exportName] : undefined;
      if (!isFunctionDefinition(definition) || definition.kind !== invocation.kind) throw new TypeError("Function kind/name mismatch");
      const argument = valueCodec(invocation.input_shape as ValueShape).decode(invocation.input);
      const db = transactionDatabase(application.schema, definition.kind === "mutation", request);
      const result = await executeDefinition(definition, Object.freeze({ db, identity: invocation.identity }), argument);
      // Result codecs and JSON serialization can invoke application getters. They
      // must never acquire a database capability after the handler has returned.
      active = false;
      if (pending) throw new Error("Await all database operations before returning");
      await tail;
      return JSON.stringify(valueCodec(invocation.result_shape as ValueShape).encode(result));
    } catch (error) {
      if (error instanceof DatabaseError) {
        // Revalidate declared fields; never serialize stack/cause or an overridden toJSON.
        const payload = JSON.stringify(new DatabaseError({ code: error.code, message: error.message, ...(error.details === undefined ? {} : { details: error.details }) }).toJSON());
        const failure = new Error("application_error");
        Object.defineProperty(failure, "application", { value: payload });
        throw failure;
      }
      const code = error instanceof Error && error.message === "schema_mismatch" ? "schema_mismatch"
        : error instanceof TypeError ? "invalid_request" : "handler_failed";
      throw new Error(code);
    } finally {
      active = false;
    }
  };
  Object.defineProperty(globalThis, "__hullaExecute", { value: execute, writable: false, configurable: false });
}
