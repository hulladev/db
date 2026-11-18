import { expect, test } from 'bun:test';
import { sqlite } from '@hulla/db-sqlite';
import { postgres } from '@hulla/db-postgres';
import { schema } from './fixtures/local-schema.ts';
import * as declarations from './fixtures/local-functions.ts';
import { defineSchema, defineTable, v } from '../src/index.ts';

test('PostgreSQL startup rejects missing or invalid explicit connections without local provisioning', async () => {
  const previous = process.env.DATABASE_URL;
  const databases: ReturnType<typeof postgres>[] = [];
  try {
    delete process.env.DATABASE_URL;
    const missing = postgres({ schema }); databases.push(missing);
    await expect(missing.ready()).rejects.toThrow('No PostgreSQL connection');
    process.env.DATABASE_URL = 'postgres://localhost/otherwise_valid';
    for (const url of ['', 'https://private-password@example.com']) {
      const invalid = postgres({ schema, url }); databases.push(invalid);
      await expect(invalid.ready()).rejects.toThrow('Invalid PostgreSQL connection URL');
    }
  } finally {
    if (previous === undefined) delete process.env.DATABASE_URL; else process.env.DATABASE_URL = previous;
    await Promise.all(databases.map(db => db.close()));
  }
});

test('shared declarations use independent instances and await startup without manual connection calls', async () => {
  const first = sqlite.initialize({ schema, filename: ':memory:', functions: () => import('./fixtures/local-catalogue.ts') });
  const second = sqlite.initialize({ schema, filename: ':memory:', functions: () => import('./fixtures/local-catalogue.ts') });
  const states: string[] = [];
  const stop = first.subscribe(state => states.push(state.status));
  try {
    // Author and execute calls while the catalogue is still loading.
    const created = await first.functions.create({ title: 'First instance' });
    expect(await first.functions.list()).toEqual([created]);
    expect(await second.functions.list()).toEqual([]);
    expect(first.functions.list).toBe(first.functions.allTasks);
    expect(first.functions.shared.list).toBe(first.functions.list);
    expect(() => first.bind(schema.query(() => 'Not registered'))).toThrow('not registered');
    const invalidNamespace = { list: declarations.list, helper: () => 'not a declaration' };
    expect(() => first.bind(invalidNamespace)).toThrow();
    expect(() => first.bind(invalidNamespace)).toThrow();
    expect(first.functions.list).not.toBe(second.functions.list);
    await expect(Promise.resolve(first.functions.invalidOutput())).rejects.toThrow();
    expect(await first.functions.list()).toEqual([created]);
    expect(states).toEqual(['initializing', 'ready']);
    let reentered = false;
    let reentrantClose: Promise<void> | undefined;
    first.subscribe(state => {
      if (state.status === 'closed' && !reentered) {
        reentered = true;
        reentrantClose = first.close();
      }
    });
    const closing = first.close();
    expect(reentrantClose).toBe(closing);
    await closing;
    expect(states).toEqual(['initializing', 'ready', 'closed']);
    await expect(Promise.resolve(first.functions.list())).rejects.toMatchObject({ code: 'disposed' });
    expect(await second.functions.list()).toEqual([]);
    await expect(first.ready()).rejects.toMatchObject({ code: 'disposed' });
  } finally { stop(); await Promise.all([first.close(), second.close()]); }
});

test('closing while the driver opens releases its connection exactly once', async () => {
  const { bunSQLiteDriver } = await import('../../db-sqlite/src/bun-driver.ts');
  const { sqliteFactory } = await import('../../db-sqlite/src/descriptor.ts');
  const { openSQLiteDatabase } = await import('../../db-sqlite/src/runtime.ts');
  const driver = await bunSQLiteDriver(':memory:');
  const entered = Promise.withResolvers<void>();
  const release = Promise.withResolvers<void>();
  let closes = 0;
  const factory = sqliteFactory(async (schema, _settings, initialize) => {
    const storage = await openSQLiteDatabase(schema, driver, {}, initialize);
    entered.resolve();
    await release.promise;
    return { ...storage, close() { closes++; return storage.close(); } };
  });
  const db = factory.initialize({ schema, filename: ':memory:' });
  await entered.promise;
  const closing = db.close();
  release.resolve();
  try {
    await closing;
    expect(closes).toBe(1);
    expect(db.state.status).toBe('closed');
  } finally { await db.close(); }
});

test('local subscriptions update after committed writes and stop when their instance closes', async () => {
  const db = sqlite.initialize({ schema, filename: ':memory:', functions: async () => declarations });
  await db.ready();
  const observed: unknown[] = [];
  const initial = Promise.withResolvers<void>();
  const updated = Promise.withResolvers<void>();
  const retired = Promise.withResolvers<void>();
  const stop = db.functions.list.subscribe(state => {
    if (state.status === 'success') {
      observed.push(state.data.map(row => row.title));
      if (state.data.length) updated.resolve(); else initial.resolve();
    } else if (state.status === 'error') retired.resolve();
  });
  try {
    await initial.promise;
    await db.functions.create({ title: 'Committed' });
    await updated.promise;
    expect(observed).toEqual([[], ['Committed']]);
    await db.close();
    await retired.promise;
  } finally { stop(); await db.close(); }
});

test('schema ownership and malformed selectors fail before invoking a storage opener', async () => {
  let opens = 0;
  const open = () => { opens++; throw new Error('Storage must not open'); };
  const other = defineSchema({ tasks: defineTable({ title: v.string() }) });
  const db = sqlite({ schema: other, open, functions: async () => declarations });
  try { await expect(db.ready()).rejects.toThrow('different schema'); }
  finally { await db.close(); }
  expect(() => sqlite({ schema, open, functions: [] } as never)).toThrow('functions');
  expect(opens).toBe(0);
});

test('operations authored during startup capture values and consumed writes cannot execute again', async () => {
  const loading = Promise.withResolvers<typeof declarations>();
  const db = sqlite.initialize({ schema, filename: ':memory:', functions: () => loading.promise });
  const input = { title: 'Captured before startup' };
  const insert = db.insert('tasks', input);
  const functionInput = { title: 'Function input before startup' };
  const create = db.functions.create(functionInput);
  class UnsupportedInput { title = 'Must not insert'; }
  const invalid = Promise.resolve(db.functions.create(new UnsupportedInput() as never)).catch(error => error);
  input.title = 'Later mutation';
  functionInput.title = 'Later function mutation';
  loading.resolve(declarations);
  try {
    const row = await insert;
    expect(row.title).toBe('Captured before startup');
    expect(await insert).toEqual(row);
    expect(() => insert.returning(['title'])).toThrow();
    const created = await create;
    expect(await invalid).toBeInstanceOf(TypeError);
    expect(created.title).toBe('Function input before startup');
    const rows = await db.select('tasks');
    expect(rows).toHaveLength(2);
    expect(rows).toContainEqual(row);
    expect(rows).toContainEqual(created);
  } finally { await db.close(); }
});
