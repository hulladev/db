import { replicaSources, type ClientStorage, type DatabaseIdentity, type QueuedMutation, type StoredSession, type StorageChange } from "@hulla/db/internal/engine";
import type { SchemaDefinition } from "@hulla/db";
import { SQLiteEngine, sqliteFailure } from "./engine.ts";
import type { SQLiteValue } from './driver.ts';

type Atomic = <T>(handler: () => Promise<T>) => Promise<T>;
type StoredSnapshot = { snapshot: Awaited<ReturnType<ClientStorage["snapshot"]>>; replica: { table: string; columns: readonly string[] }; rows: readonly Record<string, unknown>[] };

/** Persistent client state shares the connection scheduler with application transactions. */
export function sqliteClientStorage(engine: SQLiteEngine, schema: SchemaDefinition, atomic: Atomic, notify: (change: StorageChange) => void) {
  const replicas = Object.entries(schema.tables).flatMap(([name, definition]) => {
    const source = replicaSources.get(definition);
    return source ? [{ name, source }] : [];
  });
  const stores = new Map<DatabaseIdentity, ClientStorage>();
  const activeOwners = new Map<DatabaseIdentity, string>();
  let epoch = 0;
  const namespace = (group: string, key: string) => JSON.stringify([group, key]);
  async function read<T>(space: string, kind: string, key: string): Promise<T | undefined> {
    const rows = await engine.execute("SELECT value FROM _hulla_client WHERE namespace=? AND kind=? AND key=?", [space, kind, key]);
    return rows.length ? JSON.parse(String(rows[0]!.value)) as T : undefined;
  }
  async function list<T>(space: string, kind: string): Promise<T[]> {
    const rows = await engine.execute("SELECT value FROM _hulla_client WHERE namespace=? AND kind=? ORDER BY sequence", [space, kind]);
    return rows.map(row => JSON.parse(String(row.value)) as T);
  }
  async function checkCapacity(space: string, kind: string, key: string, text: string): Promise<void> {
    const bytes = new TextEncoder().encode(text).byteLength;
    const quota = await engine.execute("SELECT count(*) AS n, coalesce(sum(length(CAST(value AS BLOB))),0) AS bytes FROM _hulla_client WHERE kind=? AND NOT(namespace=? AND key=?)", [kind, space, key]);
    if (Number(quota[0]?.n) >= 128 || Number(quota[0]?.bytes) + bytes > 16 * 1024 * 1024) throw sqliteFailure("capacity", "Persistent client storage is full; acknowledge completed calls or release downloaded queries explicitly");
  }
  async function put(space: string, kind: string, key: string, value: unknown): Promise<void> {
    const text = JSON.stringify(value);
    await checkCapacity(space, kind, key, text);
    await engine.execute("INSERT INTO _hulla_client(namespace,kind,key,value) VALUES(?,?,?,?) ON CONFLICT(namespace,kind,key) DO UPDATE SET value=excluded.value", [space, kind, key, text]);
  }
  for (const table of engine.tables) if (!replicas.some(replica => replica.name === table.name) && Object.values(table.fields).some(field => typeof field.target === "string" && replicas.some(replica => replica.name === field.target)))
    throw new TypeError("Local foreign keys cannot target partially downloaded replicas; store an unconstrained scalar ID instead");
  async function rebuild(owner: DatabaseIdentity, key: string): Promise<void> {
      const targets = replicas.filter(replica => replica.source.database === owner);
      for (const replica of targets) await engine.execute(`DELETE FROM ${engine.physical(engine.table(replica.name))}`);
      const group = JSON.stringify(targets.map(replica => replica.name));
      const snapshots = await list<StoredSnapshot>(namespace(group, key), "snapshot");
      const rows = new Map<string, { revision: bigint; row: Record<string, unknown>; local: string }>();
      for (const saved of snapshots) for (const target of targets.filter(target => target.source.name === saved.replica.table && target.source.columns.every(column => saved.replica.columns.includes(column)))) {
        const table = engine.table(target.name);
        for (const row of saved.rows) {
          const identity = JSON.stringify([target.name, row[table.primaryKey]]);
          const revision = BigInt(saved.snapshot!.revision);
          if ((rows.get(identity)?.revision ?? -1n) <= revision) rows.set(identity, { revision, row, local: target.name });
        }
      }
      let current: string | undefined, insert = '';
      let names: string[] = [], values: string[] = [], parameters: SQLiteValue[] = [];
      const flush = async () => {
        if (values.length) await engine.execute(insert + values.join(','), parameters);
        values = []; parameters = [];
      };
      for (const { row, local } of rows.values()) {
        const table = engine.table(local);
        if (local !== current) {
          await flush(); current = local;
          names = Object.keys(table.fields);
          insert = `INSERT INTO ${engine.physical(table)} (${names.map(name => engine.column(table, name)).join(',')}) VALUES `;
        }
        // Bound each native call independently of the downloaded working-set size.
        if (parameters.length + names.length > 999) await flush();
        values.push(`(${names.map(name => engine.bind(table, name, row[name], parameters)).join(',')})`);
      }
      await flush();
  }
  return {
    get epoch() { return epoch; },
    isReplica: (name: string) => replicas.some(replica => replica.name === name),
    assertReadable(name: string) { const replica = replicas.find(replica => replica.name === name); if (replica && !activeOwners.has(replica.source.database)) throw sqliteFailure("replica_scope_uninitialized", "Connect an authenticated upstream before reading replica rows"); },
    factory(owner: DatabaseIdentity): ClientStorage {
      const existing = stores.get(owner);
      if (existing) return existing;
      const targets = replicas.filter(replica => replica.source.database === owner);
      if (!targets.length) throw new TypeError("The device schema has no replicas of this source database");
      const group = JSON.stringify(targets.map(replica => replica.name));
      const space = (key: string) => namespace(group, key);
      let active: string | undefined, generation = 0;
      const store: ClientStorage = {
        deactivate() { active = undefined; activeOwners.delete(owner); generation++; epoch++; notify('scope'); },
        async activate(key) {
          if (active === key) return;
          const token = ++generation; active = undefined; activeOwners.delete(owner); epoch++;
          notify('scope');
          await atomic(async () => { if (token !== generation) return; await rebuild(owner, key ?? "anonymous"); if (token !== generation) throw sqliteFailure("context_changed", "Replica scope changed"); });
          if (token !== generation) throw sqliteFailure("context_changed", "Replica scope changed");
          active = key ?? "anonymous"; activeOwners.set(owner, active);
          notify('rows');
        },
        session: key => atomic(() => read<StoredSession>(space(key), "session", "last")),
        async bind(key, session) {
          await atomic(async () => {
            const previous = await read<StoredSession>(space(key), "session", "last");
            const anchor = await read<string>(group, "source", "id");
            if (anchor && anchor !== session.source) { store.deactivate(); throw sqliteFailure("reset_required", "The replica source changed; existing rows and pending calls were retained"); }
            if (previous && (previous.scope !== session.scope || previous.principal !== session.principal)) { store.deactivate(); throw sqliteFailure("context_changed", "Verified identity changed within the same local account key"); }
            if (previous && previous.authorizationScope !== session.authorizationScope) {
              store.deactivate();
              await engine.execute("DELETE FROM _hulla_client WHERE namespace=? AND kind='snapshot'", [space(key)]);
            }
            await put(group, "source", "id", session.source);
            await put(space(key), "session", "last", session);
          });
        },
        mutations: key => atomic(() => list<QueuedMutation>(space(key), "intent")),
        async enqueue(key, mutation) {
          await atomic(async () => {
            if (await read(space(key), "intent", mutation.id)) throw sqliteFailure("request_id_reused", "Queued call already exists");
            await put(space(key), "intent", mutation.id, mutation);
          });
        },
        async update(key, mutation, previous) {
          return atomic(async () => {
            const text = JSON.stringify(mutation);
            await checkCapacity(space(key), "intent", mutation.id, text);
            return (await engine.execute("UPDATE _hulla_client SET value=? WHERE namespace=? AND kind='intent' AND key=? AND value=? RETURNING key", [text, space(key), mutation.id, JSON.stringify(previous)])).length === 1;
          });
        },
        async remove(key, id) { await atomic(async () => { await engine.execute("DELETE FROM _hulla_client WHERE namespace=? AND kind='intent' AND key=?", [space(key), id]); }); },
        snapshot: keyQuery,
        async saveSnapshot(key, query, snapshot, contract, input) {
          await store.apply(key, [{ query, snapshot, replica: contract, rows: input }]);
        },
        async apply(key, changes) {
          const token = generation;
          try {
            await atomic(async () => {
              for (const change of changes) {
                if (!change.snapshot) { await engine.execute("DELETE FROM _hulla_client WHERE namespace=? AND kind='snapshot' AND key=?", [space(key), change.query]); continue; }
                if (active !== key || token !== generation) throw sqliteFailure("context_changed", "Replica scope changed");
                const { snapshot, replica: contract, rows: input } = change;
                if (!Array.isArray(input)) throw new TypeError("Replica results must be rows");
                const matching = targets.filter(target => target.source.name === contract.table);
                if (!matching.length) continue;
                const fields = matching[0]!.source.table.fields;
                const rows = input.map(value => Object.fromEntries(contract.columns.map(name => {
                  if (!value || typeof value !== "object" || !Object.hasOwn(value, name) || !fields[name]) throw new TypeError("Invalid replica row");
                  const encoded = fields[name]!.encode((value as Record<string, unknown>)[name]);
                  return [name, typeof encoded === "bigint" ? encoded.toString() : encoded];
                })));
                const previous = await read<StoredSnapshot>(space(key), "snapshot", change.query);
                if (previous && BigInt(previous.snapshot!.revision) > BigInt(snapshot.revision)) continue;
                await put(space(key), "snapshot", change.query, { snapshot, replica: contract, rows });
              }
              if (active === key) await rebuild(owner, key);
              if (changes.some(change => change.snapshot) && token !== generation) throw sqliteFailure("context_changed", "Replica scope changed");
            });
            notify('rows');
          } catch (error) {
            // A failed new round cannot leave superseded authorization coverage active.
            try { await atomic(async () => { for (const change of changes) await engine.execute("DELETE FROM _hulla_client WHERE namespace=? AND kind='snapshot' AND key=?", [space(key), change.query]); if (active === key) await rebuild(owner, key); }); notify('rows'); }
            catch { store.deactivate(); }
            throw error;
          }
        },
        async forgetSnapshot(key, query) {
          await store.apply(key, [{ query }]);
        },
      };
      async function keyQuery(key: string, query: string) { return atomic(async () => (await read<StoredSnapshot>(space(key), "snapshot", query))?.snapshot); }
      stores.set(owner, store);
      return store;
    },
  };
}
