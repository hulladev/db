import type { DatabaseConfiguration } from "./database-configuration.ts";
import { isDatabaseSource, replicaSources, type DatabaseIdentity } from "./database-metadata.ts";
import { scopeDatabase } from "./database-scope.ts";
import { clientStorage, inheritClientStorage } from "./client-storage.ts";
import { sourceContracts } from "./client-registry.ts";
import type { HttpRuntime } from './http.ts';
import { validateRemoteUrl } from './http/url.ts';
import { authState, isAuthPromise, type ClientAuthInput } from "./auth.ts";
import type { SourceConnection } from "./connection-options.ts";
import type { ConnectedDefinition, DatabaseEngine } from "./engine.ts";
import type { SchemaDefinition } from "./schema.ts";
import type { ReplicaConnection } from "./device-connection.ts";
import { bindingInfo } from "./callable.ts";
import { DatabaseError } from "./error.ts";
import { inheritStorageExecution } from "./storage-execution.ts";

const runtimes = new WeakMap<object, (source: DatabaseIdentity) => HttpRuntime>();
export function sourceRuntime(database: object, source: DatabaseIdentity): HttpRuntime {
  const resolve = runtimes.get(database);
  if (!resolve) throw new TypeError("This database has no connected replica sources");
  return resolve(source);
}

/** Validate every source identity before handing settings to a driver. */
export async function openDatabase<S extends SchemaDefinition>(definition: DatabaseConfiguration<S>, connection: unknown, initialize = false): Promise<ConnectedDefinition<S>> {
  const required = new Map<DatabaseIdentity, string[]>();
  for (const [name, table] of Object.entries(definition.schema.tables)) {
    const source = replicaSources.get(table)?.database;
    if (source) required.set(source, [...required.get(source) ?? [], name]);
  }
  const options = connection !== null && typeof connection === "object" ? connection as Record<string, unknown> : undefined;
  const provided = options?.sources;
  if (provided !== undefined && !Array.isArray(provided)) throw new TypeError("sources must be a list of database connections");
  const sources = (provided ?? []) as SourceConnection[];
  const seen = new Set<DatabaseIdentity>();
  for (const source of sources) {
    if (!source || !isDatabaseSource(source.database)) throw new TypeError("Each source must reference a generated source contract");
    if (!required.has(source.database)) throw new TypeError("A source connection was supplied for a database that owns no replica tables in this schema");
    if (seen.has(source.database)) throw new TypeError("Duplicate source connection");
    validateRemoteUrl(source.url);
    seen.add(source.database);
  }
  for (const [source, names] of required) if (!seen.has(source)) throw new TypeError(`Missing source connection for replica tables: ${names.join(", ")}`);
  const owned: HttpRuntime[] = [];
  let local: Awaited<ReturnType<DatabaseEngine["connect"]>> | undefined;
  try {
    let settings = connection;
    if (options && Object.hasOwn(options, "sources")) { const { sources: _sources, ...driver } = options; settings = driver; }
    local = await (initialize ? definition.engine.initialize(definition.schema, settings as never) : definition.engine.connect(definition.schema, settings as never)) as typeof local;
    const scoped = scopeDatabase(local!, definition.identity);
    if (!sources.length) return scoped as ConnectedDefinition<S>;
    const { HttpRuntime } = await import('./http.ts');
    let closed = false;
    const records = sources.map(source => ({ source: { ...source }, storage: clientStorage(scoped, source.database), auth: source.auth as ClientAuthInput, update: 0, runtime: undefined as HttpRuntime | undefined }));
    const activate = async (record: typeof records[number], auth: ClientAuthInput) => {
      const update = ++record.update;
      record.auth = auth;
      record.storage.deactivate();
      try {
        const value = authState(isAuthPromise(auth) ? await auth : auth);
        if (closed || update !== record.update || record.runtime) return;
        await record.storage.activate(JSON.stringify([value?.key ?? null]));
      } catch (error) {
        if (!closed && update === record.update && !record.runtime) record.storage.deactivate();
        throw error;
      }
    };
    for (const record of records) {
      if (isAuthPromise(record.auth)) void activate(record, record.auth).catch(() => {});
      else await activate(record, record.auth ?? null);
    }
    const runtimeFor = (database: DatabaseIdentity) => {
      const record = records.find(record => record.source.database === database);
      if (closed || !record) throw new TypeError("The source is not connected to this device database");
      if (!record.runtime) {
        const { database: _database, url, ...transport } = record.source;
        const runtime = new HttpRuntime({ ...transport, auth: record.auth ?? null, baseUrl: url, contracts: sourceContracts(database), storage: record.storage });
        record.runtime = runtime;
        owned.push(runtime);
      }
      return record.runtime;
    };
    const pendingFor = async (record: typeof records[number]) => {
      const update = record.update;
      let key: string;
      if (record.runtime) {
        const auth = record.runtime.clientAuth;
        if (auth.pending || auth.error) throw auth.error ?? new Error("Authentication is still loading");
        key = JSON.stringify([auth.key]);
      } else {
        const auth = authState(isAuthPromise(record.auth) ? await record.auth : record.auth ?? null);
        key = JSON.stringify([auth?.key ?? null]);
      }
      const calls = await record.storage.mutations(key);
      const check = () => {
        const auth = record.runtime?.clientAuth;
        if (closed || update !== record.update || (auth && (auth.pending || auth.error || JSON.stringify([auth.key]) !== key)))
          throw new DatabaseError({ code: "context_changed", message: "The account changed while reading queued calls" });
      };
      return { calls: calls.map(mutation => ({ ...mutation, database: record.source.database })), check };
    };
    const operations: ReplicaConnection = {
      async pendingMutations() {
        if (closed) throw new TypeError("The device database is closed");
        const pending = await Promise.all(records.map(pendingFor));
        for (const result of pending) result.check();
        return pending.flatMap(result => result.calls);
      },
      async resolveConflict(mutation, resolution) { await runtimeFor(mutation.database).resolveConflict(mutation.id, resolution); },
      async acknowledgeMutation(mutation) { await runtimeFor(mutation.database).acknowledgeMutation(mutation.id); },
      setAuth(database, auth) {
        const record = records.find(record => record.source.database === database);
        if (!record || closed) throw new TypeError("The source is not connected to this device database");
        record.update++; record.auth = auth;
        const result = record.runtime ? record.runtime.setAuth(auth) : activate(record, auth);
        if (result) {
          const settled = Promise.resolve(result);
          void settled.catch(() => {});
          return settled;
        }
      },
      async flush() { await Promise.all(records.map(record => runtimeFor(record.source.database).flush())); },
      async forgetQuery(query, ...input) {
        const binding = bindingInfo(query);
        if (!binding.scope.databaseIdentity || binding.kind !== "query") throw new TypeError("Expected a generated query from a connected source");
        await runtimeFor(binding.scope.databaseIdentity).forgetQuery({ name: binding.name, kind: "query", input: input[0] });
      },
    };
    let closing: Promise<void> | undefined;
    const result = Object.freeze({ ...scoped, ...operations, close() {
      if (!closing) {
        closed = true;
        for (const record of records) { record.update++; record.storage.deactivate(); }
        closing = Promise.allSettled(owned.map(runtime => runtime.dispose())).then(() => scoped.close());
      }
      return closing;
    } });
    inheritClientStorage(scoped, result);
    inheritStorageExecution(scoped, result);
    runtimes.set(result, runtimeFor);
    return result as unknown as ConnectedDefinition<S>;
  } catch (error) {
    await Promise.allSettled(owned.map(runtime => runtime.dispose()));
    await local?.close();
    throw error;
  }
}
