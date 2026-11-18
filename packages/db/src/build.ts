import { glob, lstat, readFile } from 'node:fs/promises';
import { dirname, isAbsolute, relative, resolve, sep } from 'node:path';

// Runs inside Node/Vite/Rollup or Bun. Parsing belongs to the build host;
// filesystem globbing uses Node APIs so this entrypoint needs no private runtime.
interface TransformContext { parse(code: string): unknown; addWatchFile?(file: string): void }
interface ViteServer {
  watcher: { on(event: string, listener: (file: string) => void): unknown; off(event: string, listener: (file: string) => void): unknown };
  moduleGraph: { getModuleById(id: string): unknown; invalidateModule(module: never): void };
  ws: { send(message: { type: 'full-reload' }): void };
}
type Loader = 'ts' | 'tsx' | 'js' | 'jsx';
interface BunBuilder {
  onLoad(options: { filter: RegExp; namespace?: string }, callback: (args: { path: string }) => Promise<{ contents: string; loader: Loader } | undefined>): void;
}
export interface FunctionGlobsPlugin {
  readonly name: string;
  readonly enforce: 'post';
  transform(this: TransformContext, code: string, id: string): Promise<{ code: string; map: null } | null>;
  configureServer(server: ViteServer): void;
  closeBundle(): void;
  setup(build: BunBuilder): void;
}
const excluded = new Set(['_generated', 'node_modules', 'tests', 'test', '__tests__', '__mocks__']);
const ignored = (path: string) => path.split(sep).some(part => part.startsWith('.') && part !== '.' || excluded.has(part));
const codeFile = /\.[cm]?[tj]sx?$/;
function dynamicImports(node: unknown, result: string[] = []): string[] {
  if (!node || typeof node !== 'object') return result;
  if ('type' in node && node.type === 'ImportExpression' && 'source' in node) {
    const source = node.source;
    if (source && typeof source === 'object' && 'value' in source && typeof source.value === 'string') result.push(source.value);
  }
  for (const child of Object.values(node)) {
    if (Array.isArray(child)) for (const value of child) dynamicImports(value, result);
    else if (child && typeof child === 'object') dynamicImports(child, result);
  }
  return result;
}
type Tree = Map<string, Tree | string>;
function catalogue(files: readonly string[], directory: string, imports: string[], prefix: string): string {
  const tree: Tree = new Map();
  for (const file of files) {
    const path = relative(directory, file).split(sep).join('/');
    const parts = path.replace(codeFile, '').split('/');
    if (!parts.every(part => /^[\w$-]+$/.test(part) && !['constructor', '__proto__', 'prototype'].includes(part))) throw new TypeError(`Invalid function glob module: ${path}`);
    let target = tree;
    const leaf = parts.pop()!;
    for (const part of parts) {
      let child = target.get(part);
      if (typeof child === 'string') throw new TypeError(`Function glob namespace collision: ${path}`);
      if (!child) target.set(part, child = new Map());
      target = child;
    }
    if (target.has(leaf)) throw new TypeError(`Function glob namespace collision: ${path}`);
    const name = `${prefix}${imports.length}`;
    imports.push(`import * as ${name} from ${JSON.stringify(path.startsWith('.') ? path : './' + path)};`);
    target.set(leaf, name);
  }
  const object = (node: Tree): string => `{${[...node].map(([key, value]) => `${JSON.stringify(key)}:${typeof value === 'string' ? value : object(value)}`).join(',')}}`;
  return object(tree);
}

/** Expand literal functions globs using the application's existing build host. */
export function functionGlobs(): FunctionGlobsPlugin {
  const watched = new Map<string, Set<string>>();
  const cleanups = new Set<() => void>();
  const transform = async (code: string, id: string, scan: (source: string) => readonly string[], watch?: (file: string) => void) => {
    watched.delete(id);
    if (!isAbsolute(id) || !codeFile.test(id) || id.includes(`${sep}node_modules${sep}`) || !code.includes('functions')) return null;
    let prefix = '$hullaFunctions_';
    while (code.includes(prefix)) prefix += '_';
    const candidates: { start: number; end: number; marker: string }[] = [];
    // Temporary imports let the host parser distinguish code from strings,
    // comments, regexes, JSX text and type-only properties without another parser.
    const trial = code.replace(/(?:\bfunctions|["']functions["'])\s*:\s*('(?:\\.|[^'\\])*'|"(?:\\.|[^"\\])*")(?=\s*[,}])/g, (match: string, literal: string, offset: number) => {
      const start = offset + match.length - literal.length;
      const marker = prefix + candidates.length + ':';
      candidates.push({ start, end: start + literal.length, marker });
      return match.slice(0, -literal.length) + `import(${literal[0]}${marker}${literal.slice(1)})`;
    });
    if (!candidates.length) return null;
    const paths = scan(trial);
    const directory = dirname(id), imports: string[] = [], patches: { start: number; end: number; text: string }[] = [];
    const roots = new Set<string>();
    watched.set(id, roots);
    for (const candidate of candidates) {
      const pattern = paths.find(path => path.startsWith(candidate.marker))?.slice(candidate.marker.length);
      if (!pattern?.startsWith('./') || !/[*?{[]/.test(pattern)) continue;
      const staticPath = pattern.slice(0, pattern.search(/[*?{[]/));
      const root = resolve(directory, dirname(staticPath + (staticPath.endsWith('/') ? '_' : '')));
      roots.add(root); watch?.(root);
      const files: string[] = [];
      for await (const path of glob(pattern, { cwd: directory })) {
        if (ignored(path) || !codeFile.test(path) || /\.(?:d|test|spec)\.[cm]?[tj]sx?$/.test(path)) continue;
        const file = resolve(directory, path);
        if (file !== id && (await lstat(file)).isFile()) files.push(file);
      }
      if (!files.length) throw new TypeError(`Function glob matched no files: ${pattern}`);
      for (const file of files) watch?.(file);
      patches.push({ ...candidate, text: `()=>Promise.resolve(${catalogue(files.sort(), directory, imports, prefix)})` });
    }
    if (!patches.length) { watched.delete(id); return null; }
    for (const patch of patches.reverse()) code = code.slice(0, patch.start) + patch.text + code.slice(patch.end);
    const insertion = code.startsWith('#!') ? code.indexOf('\n') + 1 : 0;
    return { code: code.slice(0, insertion) + imports.join('') + code.slice(insertion), map: null };
  };
  return {
    name: 'hulla-function-globs', enforce: 'post',
    transform(code, id) { return transform(code, id.split('?')[0]!, source => dynamicImports(this.parse(source)), file => this.addWatchFile?.(file)); },
    configureServer(server) {
      const membership = (file: string) => {
        if (!codeFile.test(file)) return;
        let changed = false;
        for (const [id, roots] of watched) if ([...roots].some(root => { const path = relative(root, file); return !isAbsolute(path) && !ignored(path); })) {
          const module = server.moduleGraph.getModuleById(id);
          if (module) { server.moduleGraph.invalidateModule(module as never); changed = true; }
        }
        if (changed) server.ws.send({ type: 'full-reload' });
      };
      server.watcher.on('add', membership); server.watcher.on('unlink', membership);
      cleanups.add(() => { server.watcher.off('add', membership); server.watcher.off('unlink', membership); });
    },
    closeBundle() { for (const cleanup of cleanups) cleanup(); cleanups.clear(); watched.clear(); },
    setup(build) {
      build.onLoad({ filter: codeFile, namespace: 'file' }, async ({ path }) => {
        if (path.includes(`${sep}node_modules${sep}`)) return undefined;
        const loader: Loader = /\.[cm]?tsx?$/.test(path) ? path.endsWith('x') ? 'tsx' : 'ts' : path.endsWith('x') ? 'jsx' : 'js';
        const result = await transform(await readFile(path, 'utf8'), path, source => new Bun.Transpiler({ loader }).scanImports(source).filter(value => value.kind === 'dynamic-import').map(value => value.path));
        return result ? { contents: result.code, loader } : undefined;
      });
    },
  };
}
