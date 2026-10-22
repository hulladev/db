import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { renameSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { createDatabase } from '@hulla/db/server';
import { defineSchema, defineTable, v } from '@hulla/db/schema';
import * as browser from '@hulla/db';
const module = new URL('./native.js', import.meta.resolve('@hulla/db/server'));
const { resolveNativeExecutable } = await import(module);
assert.equal(typeof browser.DatabaseError, 'function');
assert.equal(typeof createDatabase, 'function');
assert.ok(defineSchema({ notes: defineTable({ title: v.string() }) }));
for (const kind of ['bridge', 'service', 'js']) {
  const path = resolveNativeExecutable(kind);
  assert.ok(path.startsWith(fileURLToPath(new URL('../native/', module))));
  // Allow cold executable startup; this checks liveness, not startup latency.
  const result = spawnSync(path, [], { input: '', timeout: 15000, env: {} });
  assert.equal(result.error, undefined);
  assert.notEqual(result.signal, 'SIGTERM');
}
const native = fileURLToPath(new URL('../native/', module)).slice(0, -1);
renameSync(native, native + '.hidden');
assert.throws(() => resolveNativeExecutable('bridge'), /No packaged hulladb-bridge executable/);
process.env.HULLADB_ENGINE = '/explicit/engine';
assert.equal(resolveNativeExecutable('bridge'), '/explicit/engine');
console.log('Installed package imports, native startup, missing asset failure and override passed under Node');
