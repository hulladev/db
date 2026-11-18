import { expect, test } from 'bun:test';
import { mkdtemp, mkdir, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { functionGlobs } from '../src/build.ts';
import { parse } from 'acorn';
import { runtimeExportConditions } from '../src/runtime-conditions.ts';

async function fixture(legacy = false) {
  const directory = await mkdtemp(new URL('./.glob-build-', import.meta.url).pathname);
  await mkdir(join(directory, 'ops'));
  await Bun.write(join(directory, 'tsconfig.json'), JSON.stringify({ extends: '../../tsconfig.json', compilerOptions: { allowJs: true, checkJs: true }, include: ['**/*'] }));
  await Bun.write(join(directory, 'schema.ts'), `import {defineSchema,defineTable,v} from '@hulla/db'; export const schema=defineSchema({tasks:defineTable({title:v.string()})}); export const {query,mutation}=schema;`);
  await Bun.write(join(directory, 'ops/tasks.' + (legacy ? 'cjs' : 'ts')), legacy
    ? `const {query}=require('../schema.ts'); module.exports={list:query(({db})=>db.select('tasks'))};`
    : `import {query} from '../schema'; export const list=query(({db})=>db.select('tasks')); export {list as all};`);
  await Bun.write(join(directory, 'app.ts'), `import {sqlite} from '@hulla/db-sqlite';import {schema}from'./schema';export const db=sqlite.initialize({schema,filename:':memory:',functions:'./ops/*.${legacy ? 'cjs' : 'ts'}'});`);
  return directory;
}

test('Bun builds glob catalogues without executing startup and runs typed local declaration bindings', async () => {
  const directory = await fixture();
  try {
    const entry = join(directory, 'app.ts');
    await Bun.write(join(directory, 'options.ts'), `import{schema}from'./schema';export const options={schema,filename:':memory:',functions:'./ops/*.ts'};`);
    await Bun.write(entry, `import{sqlite}from'@hulla/db-sqlite';import{options}from'./options';export const db=sqlite.initialize(options);`);
    await Bun.write(entry, (await Bun.file(entry).text()) + `\nimport {list}from'./ops/tasks';const input={title:'Bundled'};const pending=db.insert('tasks',input);input.title='Changed';const row=await pending;const run=db.bind(list);if((await run())[0]?.title!=='Bundled')throw new Error('Wrong context');if(db.functions.ops.tasks.list!==db.functions.ops.tasks.all)throw new Error('Alias identity');await db.close();`);
    const build = await Bun.build({ entrypoints: [entry], target: 'bun', conditions: runtimeExportConditions(), plugins: [functionGlobs()] });
    expect(build.success).toBe(true);
    const path = join(directory, 'app.js'); await Bun.write(path, build.outputs[0]!);
    const child = Bun.spawn([process.execPath, path], { stdout: 'pipe', stderr: 'pipe', timeout: 10_000 });
    const [exit, stderr] = await Promise.all([child.exited, new Response(child.stderr).text()]);
    expect({ exit, stderr: exit ? stderr : '' }).toEqual({ exit: 0, stderr: '' });
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test('Node build hooks transform CommonJS declarations and track additions without importing startup', async () => {
  const directory = await fixture(true);
  try {
    const entry = join(directory, 'app.ts');
    await Bun.write(entry, (await Bun.file(entry).text()) + `\nthrow new Error('BUILD_MUST_NOT_EXECUTE_APP');`);
    const plugin = functionGlobs();
    const watches: string[] = [];
    const transformed = await plugin.transform.call({ parse: code => parse(code, { ecmaVersion: 'latest', sourceType: 'module' }), addWatchFile: file => watches.push(file) }, await Bun.file(entry).text(), entry);
    expect(transformed?.code).toContain('Promise.resolve');
    expect(watches).toContain(join(directory, 'ops/tasks.cjs'));
    expect(watches).toContain(join(directory, 'ops'));
    const events = new Map<string, (file: string) => void>();
    let invalidations = 0;
    plugin.configureServer({ watcher: { on(event, listener) { events.set(event, listener); }, off(event) { events.delete(event); } }, moduleGraph: { getModuleById: () => ({}), invalidateModule() { invalidations++; } }, ws: { send() {} } });
    events.get('add')!(join(directory, 'ops/new.ts'));
    expect(invalidations).toBe(1);
    await plugin.transform.call({ parse: code => parse(code, { ecmaVersion: 'latest', sourceType: 'module' }) }, 'export const nothing = 1', entry);
    events.get('add')!(join(directory, 'ops/later.ts'));
    expect(invalidations).toBe(1);
    await plugin.closeBundle();
    expect(events.size).toBe(0);
    const host = join(directory, 'node-host.ts');
    await Bun.write(host, `import {functionGlobs}from ${JSON.stringify(new URL('../src/build.ts', import.meta.url).pathname)};import{readFile}from'node:fs/promises';import{parse}from'acorn';const p=${JSON.stringify(entry)};const result=await functionGlobs().transform.call({parse:code=>parse(code,{ecmaVersion:'latest',sourceType:'module'})},await readFile(p,'utf8'),p);if(!result?.code.includes('Promise.resolve'))throw new Error('No transform');`);
    const built = await Bun.build({ entrypoints: [host], target: 'node', packages: 'external', conditions: runtimeExportConditions() });
    expect(built.success).toBe(true);
    const hostPath = join(directory, 'node-host.mjs'); await Bun.write(hostPath, built.outputs[0]!);
    const child = Bun.spawn([Bun.which('node')!, hostPath], { env: { ...process.env, PATH: '' }, stdout: 'pipe', stderr: 'pipe', timeout: 5000 });
    const [exit, stderr] = await Promise.all([child.exited, new Response(child.stderr).text()]);
    expect({ exit, stderr: exit ? stderr : '' }).toEqual({ exit: 0, stderr: '' });
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test('foreign-schema glob functions reject at runtime before opening storage', async () => {
  const directory = await fixture();
  try {
    await Bun.write(join(directory, 'ops/foreign.ts'), `import{defineSchema,defineTable,v}from'@hulla/db';const other=defineSchema({private:defineTable({secret:v.string()})});export const leak=(()=>other.query(()=>{throw new Error('MUST_NOT_EXECUTE_HANDLER')}))();`);
    const entry = join(directory, 'app.ts');
    await Bun.write(entry, (await Bun.file(entry).text()).replace("filename:':memory:'", "open:()=>{globalThis.storageOpened=true;throw new Error('Must reject before storage')}") + `\ntry{await db.ready();throw new Error('Accepted foreign function')}catch(error){if(!error.message.includes('different schema'))throw error}if(globalThis.storageOpened)throw new Error('Opened storage');await db.close();`);
    const build = await Bun.build({ entrypoints: [entry], target: 'bun', conditions: runtimeExportConditions(), plugins: [functionGlobs()] });
    expect(build.success).toBe(true);
    const path = join(directory, 'foreign.js'); await Bun.write(path, build.outputs[0]!);
    const child = Bun.spawn([process.execPath, path], { stdout: 'pipe', stderr: 'pipe', timeout: 5000 });
    const [exit, stderr] = await Promise.all([child.exited, new Response(child.stderr).text()]);
    expect({ exit, stderr: exit ? stderr : '' }).toEqual({ exit: 0, stderr: '' });
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test('empty globs watch their roots and recover when the first file appears', async () => {
  const directory = await fixture();
  const plugin = functionGlobs();
  try {
    const entry = join(directory, 'app.ts');
    const empty = (await Bun.file(entry).text()).replace('./ops/*.ts', './missing/*.ts');
    const watches: string[] = [];
    const events = new Map<string, (file: string) => void>();
    let invalidations = 0;
    plugin.configureServer({ watcher: { on(event, listener) { events.set(event, listener); }, off(event) { events.delete(event); } }, moduleGraph: { getModuleById: () => ({}), invalidateModule() { invalidations++; } }, ws: { send() {} } });
    await expect(plugin.transform.call({ parse: code => parse(code, { ecmaVersion: 'latest', sourceType: 'module' }), addWatchFile: file => watches.push(file) }, empty, entry)).rejects.toThrow('matched no files');
    expect(watches).toContain(join(directory, 'missing'));
    events.get('add')!(join(directory, 'missing/first.ts'));
    expect(invalidations).toBe(1);
    await mkdir(join(directory, 'missing'));
    await Bun.write(join(directory, 'missing/first.ts'), `export{list}from'../ops/tasks'`);
    expect(await plugin.transform.call({ parse: code => parse(code, { ecmaVersion: 'latest', sourceType: 'module' }) }, empty, entry)).not.toBeNull();
    plugin.closeBundle();
    expect(events.size).toBe(0);
  } finally { plugin.closeBundle(); await rm(directory, { recursive: true, force: true }); }
});

test('CommonJS application initializers compile and execute without an ESM registration barrel', async () => {
  const directory = await fixture(true);
  try {
    const entry = join(directory, 'app.cjs');
    await Bun.write(entry, `const {sqlite}=require('@hulla/db-sqlite');const {schema}=require('./schema.ts');const {list}=require('./ops/tasks.cjs');module.exports={db:sqlite.initialize({schema,filename:':memory:',functions:'./ops/*.cjs'})};const db=module.exports.db;(async()=>{await db.insert('tasks',{title:'CJS'});if((await db.bind(list)())[0]?.title!=='CJS')throw new Error('Wrong database');await db.close()})().catch(error=>{console.error(error);process.exitCode=1});`);
    const build = await Bun.build({ entrypoints: [entry], target: 'node', conditions: runtimeExportConditions(), plugins: [functionGlobs()], throw: false });
    expect(build.logs.filter(log => log.level === 'error').map(log => log.message)).toEqual([]);
    expect(build.success).toBe(true);
    const path = join(directory, 'app.mjs'); await Bun.write(path, build.outputs[0]!);
    const child = Bun.spawn([process.env.NODE ?? 'node', path], { stdout: 'pipe', stderr: 'pipe', timeout: 10_000 });
    const [exit, stderr] = await Promise.all([child.exited, new Response(child.stderr).text()]);
    expect({ exit, stderr: exit ? stderr : '' }).toEqual({ exit: 0, stderr: '' });
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test('browser globs import matched modules without executing them during build or including tooling', async () => {
  const directory = await fixture();
  try {
    const entry = join(directory, 'app.ts');
    await Bun.write(entry, (await Bun.file(entry).text()).replace("filename:':memory:'", "open:()=>{throw new Error('Provide WASM in the running app')}"));
    await Bun.write(join(directory, 'ops/helper.ts'), `throw new Error('MATCHED_HELPER_RUNS_WITH_APP');export const ordinaryHelper=()=>123;`);
    await Bun.write(join(directory, 'ops/tasks.ts'), (await Bun.file(join(directory, 'ops/tasks.ts')).text()) + `\nthrow new Error('SELECTED_MODULE_MUST_NOT_EXECUTE_DURING_BUILD');`);
    const build = await Bun.build({ entrypoints: [entry], target: 'browser', conditions: runtimeExportConditions(), plugins: [functionGlobs()], metafile: true, throw: false });
    expect(build.logs.filter(log => log.level === 'error').map(log => log.message)).toEqual([]);
    expect(build.success).toBe(true);
    const output = await build.outputs[0]!.text();
    expect(output).toContain('MATCHED_HELPER_RUNS_WITH_APP');
    expect(Object.keys(build.metafile!.inputs).some(path => /(?:development-compiler|function-analysis|node-driver|bun-driver)/.test(path))).toBe(false);
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test('glob expansion preserves callbacks, strings, regexes, template text and type declarations', async () => {
  const directory = await fixture();
  try {
    const entry = join(directory, 'app.ts');
    const code = (await Bun.file(entry).text()).replace("'./ops/*.ts'", "async()=>{const module=await import('./ops/tasks');return {queries:{list:module.list}}}");
    await Bun.write(entry, code + `
export const text="functions: './missing/*.ts'";
export const pattern=/functions: '.nothing*'/;
export const template=\`functions: './missing/*.ts'\`;
type Selector={functions:'./missing/*.ts'};
// functions: './missing/*.ts'
await db.ready();if(text!==template)throw new Error('Changed text');await db.close();
`);
    const build = await Bun.build({ entrypoints: [entry], target: 'bun', conditions: runtimeExportConditions(), plugins: [functionGlobs()] });
    expect(build.success).toBe(true);
    const path = join(directory, 'preserved.js'); await Bun.write(path, build.outputs[0]!);
    const child = Bun.spawn([process.execPath, path], { stdout: 'pipe', stderr: 'pipe', timeout: 5000 });
    const [exit, stderr] = await Promise.all([child.exited, new Response(child.stderr).text()]);
    expect({ exit, stderr: exit ? stderr : '' }).toEqual({ exit: 0, stderr: '' });
  } finally { await rm(directory, { recursive: true, force: true }); }
});
