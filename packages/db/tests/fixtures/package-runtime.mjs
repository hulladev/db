import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { renameSync } from 'node:fs';
import { dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createDatabase } from '@hulla/db-postgres/server';
import { defineSchema, defineTable, v } from '@hulla/db/schema';
import * as browser from '@hulla/db';
const module = new URL('./native.js', import.meta.resolve('@hulla/db-postgres/server'));
const { resolveNativeExecutable } = await import(module);
assert.equal(typeof browser.DatabaseError, 'function');
assert.equal(typeof createDatabase, 'function');
assert.ok(defineSchema({ notes: defineTable({ title: v.string() }) }));
const platform = `${process.platform}-${process.arch}`;
const native = dirname(fileURLToPath(import.meta.resolve(`@hulla/db-postgres-${platform}/hulladb-bridge`)));
for (const kind of ['bridge', 'service', 'js']) {
  const path = resolveNativeExecutable(kind);
  assert.ok(path.startsWith(native + '/'));
  // Allow cold executable startup; this checks liveness, not startup latency.
  const result = spawnSync(path, [], { input: '', timeout: 15000, env: {} });
  assert.equal(result.error, undefined);
  assert.notEqual(result.signal, 'SIGTERM');
}
renameSync(native, native + '.hidden');
try { assert.throws(() => resolveNativeExecutable('bridge'), /No packaged hulladb-bridge executable/); }
finally { renameSync(native + '.hidden', native); }
process.env.HULLADB_ENGINE = '/explicit/engine';
assert.equal(resolveNativeExecutable('bridge'), '/explicit/engine');
console.log(`Installed package imports, native startup, missing asset failure and override passed under ${process.versions.bun ? 'Bun' : 'Node'}`);
