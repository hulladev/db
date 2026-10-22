import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { cp, mkdir, mkdtemp, readFile, readdir, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createServer } from 'node:net';

// A local deployment-shape proof. No cloud account or provider resource is used.
const tarball = process.argv[2];
if (!tarball || !process.env.HULLADB_TEST_URL) throw new Error('Usage: HULLADB_TEST_URL=... bun deploy/cloud/verify.ts /absolute/hulla-db.tgz');
const source = fileURLToPath(new URL('./next-app', import.meta.url));
const root = await mkdtemp(join(tmpdir(), 'hulla-cloud-proof-'));
const app = join(root, 'build'), runtime = join(root, 'runtime');
const namespace = `cloud_fixture_${Date.now()}_${process.pid}`;
const cookie = `fixture-${crypto.randomUUID()}-${crypto.randomUUID()}`;
const packageHash = createHash('sha256').update(await readFile(resolve(tarball))).digest('hex');
const nodeLookup = Bun.spawnSync([process.env.NODE_BINARY ?? Bun.which('node')!, '-p', 'process.execPath']);
if (nodeLookup.exitCode !== 0) throw new Error('A working Node binary is required');
const node = nodeLookup.stdout.toString().trim();
async function freePort() {
  const server = createServer();
  await new Promise<void>((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  const port = (server.address() as { port: number }).port;
  await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
  return port;
}
const [webPort, servicePort] = await Promise.all([freePort(), freePort()]);
const origin = `http://127.0.0.1:${webPort}`;
const env = { ...process.env, NEXT_TELEMETRY_DISABLED: '1', FIXTURE_MODE: 'isolated', DATABASE_URL: process.env.HULLADB_TEST_URL,
  DATABASE_NAMESPACE: namespace, FIXTURE_SESSION_TOKEN: cookie, FIXTURE_SESSION_STATUS_FILE: join(root, 'session-state'), APP_ORIGIN: origin,
  SERVICE_PORT: String(servicePort), NEXT_PUBLIC_HULLA_SERVICE_URL: `http://127.0.0.1:${servicePort}` };
// Do not permit checkout binary overrides to conceal a broken package.
for (const key of ['HULLADB_ENGINE', 'HULLADB_SERVICE', 'HULLADB_JS']) delete (env as Record<string, string | undefined>)[key];
async function command(args: string[], cwd = app) {
  const child = Bun.spawn(args, { cwd, env, stdout: 'pipe', stderr: 'pipe' });
  const [out, error, status] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited]);
  if (status !== 0) throw new Error(`${args[0]} failed (${status}):\n${out}\n${error}`);
  return out;
}
let service: ReturnType<typeof Bun.spawn> | undefined, web: ReturnType<typeof Bun.spawn> | undefined;
let browser: { close(): Promise<void> } | undefined;
let serviceLog = '', webLog = '';
async function capture(stream: ReadableStream<Uint8Array>, append: (text: string) => void) {
  for await (const chunk of stream) append(new TextDecoder().decode(chunk));
}
async function waitFor(url: string) {
  for (let attempt = 0; attempt < 200; attempt++) {
    try { if ((await fetch(url)).status < 500) return; } catch {}
    await Bun.sleep(50);
  }
  throw new Error(`Server did not become ready: ${webLog}\n${serviceLog}`);
}
async function files(root: string): Promise<string[]> {
  return (await Promise.all((await readdir(root, { withFileTypes: true })).map(entry =>
    entry.isDirectory() ? files(join(root, entry.name)) : [join(root, entry.name)]))).flat();
}
try {
  await cp(source, app, { recursive: true, filter: path => !path.split(/[\\/]/).some(part => ['node_modules', '.next', '_generated', 'hulla-db.tgz'].includes(part)) });
  // Content-address the local archive: a previous lockfile must not reuse an
  // older unpublished tarball with the same package name/version.
  const archive = `hulla-db-${packageHash}.tgz`;
  await cp(resolve(tarball), join(app, archive));
  const packagePath = join(app, 'package.json');
  const manifest = JSON.parse(await readFile(packagePath, 'utf8'));
  manifest.dependencies['@hulla/db'] = `file:./${archive}`;
  await writeFile(packagePath, JSON.stringify(manifest, null, 2));
  await writeFile(env.FIXTURE_SESSION_STATUS_FILE, 'active');
  console.log(`Fixture workspace: ${root}`);
  await command([process.execPath, 'install', '--ignore-scripts']);
  await command([process.execPath, '--bun', 'node_modules/@hulla/db/dist/cli.js', 'generate']);
  await command([node, 'node_modules/@hulla/db/dist/cli.js', 'init']);
  console.log('Installed package generated and initialized outside the checkout.');
  console.log(await command([node, 'node_modules/next/dist/bin/next', 'build', '--webpack']));
  await cp(join(app, '.next/standalone'), runtime, { recursive: true });
  await mkdir(join(runtime, '.next'), { recursive: true });
  await cp(join(app, '.next/static'), join(runtime, '.next/static'), { recursive: true });
  const native = join(runtime, `node_modules/@hulla/db/native/${process.platform}-${process.arch}/hulladb-bridge`);
  assert((await readFile(native)).length > 0, 'Standalone tracing omitted the native bridge');
  assert((await readFile(join(runtime, 'node_modules/@hulla/db/package.json'), 'utf8')).includes('dist/server.js'));
  for (const path of await files(join(runtime, '.next/static'))) {
    const content = await readFile(path, 'utf8');
    assert(!content.includes(cookie) && !content.includes(env.DATABASE_URL!), 'Private credential leaked into browser asset');
  }
  const runtimeEnv = { ...env, PATH: `${dirname(node)}:/usr/bin:/bin` };
  service = Bun.spawn([node, join(app, 'node_modules/@hulla/db/dist/cli.js'), 'serve', join(app, 'db.config.ts')], { cwd: app, env: runtimeEnv, stdout: 'pipe', stderr: 'pipe' });
  void capture(service.stdout as ReadableStream<Uint8Array>, text => { serviceLog += text; });
  void capture(service.stderr as ReadableStream<Uint8Array>, text => { serviceLog += text; });
  web = Bun.spawn([node, 'server.js'], { cwd: runtime, env: { ...runtimeEnv, HOSTNAME: '127.0.0.1', PORT: String(webPort) }, stdout: 'pipe', stderr: 'pipe' });
  void capture(web.stdout as ReadableStream<Uint8Array>, text => { webLog += text; });
  void capture(web.stderr as ReadableStream<Uint8Array>, text => { webLog += text; });
  await waitFor(origin);
  await waitFor(`${env.NEXT_PUBLIC_HULLA_SERVICE_URL}/functions/session`);
  assert.equal((await fetch(`${origin}/api/tasks`)).status, 401);
  const headers = { cookie: `fixture_session=${cookie}`, origin, 'content-type': 'application/json' };
  let response = await fetch(`${origin}/api/tasks`, { method: 'POST', headers, body: JSON.stringify({ title: 'SSR seeded through route' }) });
  assert.equal(response.status, 200, await response.text());
  const html = await (await fetch(origin, { headers })).text();
  assert(html.includes('SSR seeded through route'), 'SSR did not read committed rows');
  const require = createRequire(join(app, 'package.json'));
  const { chromium } = require('playwright-core') as typeof import('./next-app/node_modules/playwright-core');
  browser = await chromium.launch({ headless: true, ...(process.env.CHROMIUM_EXECUTABLE ? { executablePath: process.env.CHROMIUM_EXECUTABLE } : {}) });
  const context = await (browser as Awaited<ReturnType<typeof chromium.launch>>).newContext();
  await context.addCookies([{ name: 'fixture_session', value: cookie, url: origin, httpOnly: true, sameSite: 'Lax' }]);
  const page = await context.newPage();
  const browserErrors: string[] = [];
  page.on('pageerror', error => browserErrors.push(error.message));
  const networkErrors: string[] = [];
  page.on('requestfailed', request => {
    const error = request.failure()?.errorText;
    if (error !== 'net::ERR_ABORTED') networkErrors.push(`${request.url()}: ${error}`);
  });
  page.on('response', response => { if (response.url().startsWith(env.NEXT_PUBLIC_HULLA_SERVICE_URL) && response.status() >= 400) networkErrors.push(`${response.url()}: ${response.status()}`); });
  await page.goto(origin);
  await page.waitForFunction(() => document.querySelector('[data-testid=live-status]')?.textContent === 'success').catch(async error => { throw new Error(`${error.message}; page=${await page.locator('body').innerText()}; errors=${JSON.stringify(browserErrors)}; network=${JSON.stringify(networkErrors)}`); });
  await page.getByLabel('Task title').fill('Created through server action');
  await page.getByRole('button', { name: 'Create through server action' }).click();
  await page.waitForFunction(() => document.querySelector('[data-testid=live-tasks]')?.textContent?.includes('Created through server action'));
  response = await fetch(`${origin}/api/tasks`, { method: 'POST', headers, body: JSON.stringify({ title: 'Direct route reaches live query' }) });
  assert.equal(response.status, 200, await response.text());
  await page.waitForFunction(() => document.querySelector('[data-testid=live-tasks]')?.textContent?.includes('Direct route reaches live query'));
  response = await fetch(`${origin}/api/tasks?rollback=true`, { method: 'POST', headers, body: JSON.stringify({ title: 'Must rollback' }) });
  assert.equal(response.status, 409, await response.text());
  const rows = await (await fetch(`${origin}/api/tasks`, { headers })).json() as { title: string }[];
  assert(!rows.some(row => row.title.includes('Must rollback')));
  await page.reload();
  await page.waitForFunction(() => document.querySelector('[data-testid=live-status]')?.textContent === 'success');
  assert(!(await page.getByTestId('live-tasks').textContent())?.includes('Must rollback'));
  assert.deepEqual(browserErrors, []);
  assert.deepEqual(networkErrors, [], 'Unexpected browser transport failure before session revocation');
  await writeFile(env.FIXTURE_SESSION_STATUS_FILE, 'revoked');
  await page.waitForFunction(() => document.querySelector('[data-testid=live-status]')?.textContent?.startsWith('error:'));
  assert.equal(await page.getByTestId('live-tasks').textContent(), '', 'Revoked live query retained protected rows');
  await writeFile(join(root, 'result.json'), JSON.stringify({ namespace, packageSha256: packageHash, node: await command([node, '--version']), next: '16.4.2', react: '19.2.5', package: '0.1.0', tracedBridge: native, checks: ['standalone tracing', 'SSR', 'route write', 'server action write', 'grouped rollback', 'browser live updates', 'anonymous rejection', 'revoked live query clears data', 'no browser credentials'], deployment: false }, null, 2));
  console.log(`Cloud-shape local proof passed. Evidence: ${join(root, 'result.json')}`);
} catch (error) {
  console.error(`Service output: ${serviceLog}\nNext output: ${webLog}`);
  throw error;
} finally {
  await browser?.close();
  service?.kill('SIGTERM'); web?.kill('SIGTERM');
  if (service) await service.exited;
  if (web) await web.exited;
  // Disposable storage cleanup is explicit and scoped to this generated namespace.
  const psql = process.env.PSQL;
  if (psql) await command([psql, env.DATABASE_URL!, '-v', 'ON_ERROR_STOP=1', '-c', `DROP SCHEMA IF EXISTS "${namespace}" CASCADE;`]);
  else console.log(`Cleanup namespace explicitly after inspection: ${namespace}`);
}
