import { timingSafeEqual } from 'node:crypto';
import { readFileSync } from 'node:fs';

/** Fixture boundary only: replace this with your auth provider's server verifier. */
export function fixtureSettings() {
  const namespace = process.env.DATABASE_NAMESPACE;
  const cookie = process.env.FIXTURE_SESSION_TOKEN;
  if (process.env.FIXTURE_MODE !== 'isolated' || !namespace?.startsWith('cloud_fixture_')
    || !cookie || cookie.length < 32 || cookie.startsWith('replace-with-') || !process.env.DATABASE_URL || !process.env.APP_ORIGIN) {
    throw new Error('Cloud fixture requires explicitly isolated storage and a private session token.');
  }
  return { namespace, cookie, databaseUrl: process.env.DATABASE_URL, origin: process.env.APP_ORIGIN };
}
export function verifySession(headers: Headers): { id: string } | null {
  const expected = Buffer.from(fixtureSettings().cookie);
  const value = headers.get('cookie')?.split(';').map(item => item.trim()).find(item => item.startsWith('fixture_session='))?.slice('fixture_session='.length);
  if (value === undefined) return null;
  const actual = Buffer.from(value);
  if (actual.length !== expected.length || !timingSafeEqual(actual, expected)) throw new Error('Invalid session');
  // Optional test-provider state lets the harness revoke an already-open stream.
  const statusFile = process.env.FIXTURE_SESSION_STATUS_FILE;
  if (statusFile && readFileSync(statusFile, 'utf8').trim() !== 'active') throw new Error('Session revoked');
  return { id: 'fixture-user' };
}
