import { expect, test } from 'bun:test';
import { compileChecks, sql } from '../src/sql-check.ts';

test('SQL checks keep authored text, column names and untrusted scalar contents separate', () => {
  const injection = "x'); DROP TABLE orders; --";
  const checks = compileChecks(row => ({
    valid: sql`${sql`${row['unit price']} >= ${0}`} AND ${row.label} <> ${injection}`,
  }), ['unit price', 'label'], 'id');
  expect(checks).toEqual([{
    name: 'valid', parts: ['', ' >= ', ' AND ', ' <> ', ''],
    operands: [{ field: 'unit price' }, { value: 0 }, { field: 'label' }, { value: injection }],
  }]);
});

test('SQL checks reject invalid interpolations and statements before generation', () => {
  expect(() => sql`true; DROP TABLE orders`).toThrow('one expression');
  expect(() => sql`true -- hidden expression`).toThrow('one expression');
  expect(() => sql`${undefined as never}`).toThrow('scalar literal');
  expect(() => sql`${NaN}`).toThrow('scalar literal');
  expect(() => sql`${1n << 63n}`).toThrow('scalar literal');
  expect(() => compileChecks(() => ({ empty: sql`` }), [], 'id')).toThrow('empty');
  let accessed = false;
  expect(() => compileChecks(() => ({ get invalid() { accessed = true; return sql`true`; } }), [], 'id')).toThrow('accessors');
  expect(accessed).toBe(false);
});
