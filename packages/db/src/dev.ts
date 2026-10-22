import { lstatSync, watch, type FSWatcher } from "node:fs";
import { dirname, isAbsolute, relative, resolve, sep } from "node:path";
import { runtimeExportConditions } from "./runtime-conditions.ts";

const ignored = new Set([
  "_generated", "node_modules", ".git", "dist", "build", "target", "coverage",
  ".next", ".nuxt", ".output", ".svelte-kit", ".astro", ".solid", ".cache",
  ".turbo", ".vite", ".vercel", ".netlify",
]);
function contains(parent: string, child: string): boolean {
  const path = relative(parent, child);
  return path === "" || (!isAbsolute(path) && path !== ".." && !path.startsWith(`..${sep}`));
}
function relevant(directory: string, path: string, event: string): boolean {
  const parts = path.split(/[\\/]/);
  if (parts.some(part => ignored.has(part))) return false;
  const name = parts.at(-1)!;
  if (/\.(?:log|tmp|swp|swo|map|tsbuildinfo)$/.test(name) || name === ".DS_Store") return false;
  if (/\.(?:[cm]?[jt]sx?|jsonc?|ya?ml|toml)$/.test(name) || name === ".env" || name.startsWith(".env.")
    || ["bun.lock", "bun.lockb", "yarn.lock"].includes(name)) return true;
  if (event !== "rename") return false;
  // A newly created/moved directory can introduce declarations at any depth.
  // Deleted paths cannot be classified, so conservatively refresh discovery.
  try { return lstatSync(resolve(directory, path)).isDirectory(); }
  catch { return true; }
}

/** Development-only supervisor. Each generation gets a fresh module cache. */
export async function startDevelopment(configPath: string): Promise<{ close(): Promise<void>; closed: Promise<void> }> {
  if (typeof Bun === "undefined") throw new Error("hulla-db dev requires Bun. Use Node or Bun with hulla-db serve for generated applications.");
  const config = resolve(configPath), root = dirname(config);
  const watchers = new Map<string, FSWatcher>();
  let child: Bun.Subprocess<"ignore", "inherit", "inherit"> | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let stopped = false, revision = 0, chain = Promise.resolve();
  let finish!: () => void;
  const closed = new Promise<void>(resolve => { finish = resolve; });

  async function stopChild() {
    if (!child) return;
    const previous = child;
    child = undefined;
    previous.kill("SIGTERM");
    const force = setTimeout(() => previous.kill("SIGKILL"), 5000);
    try { await previous.exited; } finally { clearTimeout(force); }
  }
  function schedule() {
    if (stopped) return;
    const next = ++revision;
    clearTimeout(timer);
    timer = setTimeout(() => {
      chain = chain.then(async () => {
        await stopChild();
        if (stopped || next !== revision) return;
        launch();
      }).catch(() => { console.error("Development restart failed; waiting for the next source change."); });
    }, 100);
  }
  function watchDirectory(directory: string) {
    const path = resolve(directory);
    if ([...watchers.keys()].some(parent => contains(parent, path))) return;
    try {
      const watcher = watch(path, { recursive: true }, (event, filename) => {
        if (filename === null || relevant(path, String(filename), event)) schedule();
      });
      watcher.on("error", () => {
        watcher.close(); watchers.delete(path);
        console.error(`Cannot watch ${path}; restart dev after restoring this directory.`);
      });
      watchers.set(path, watcher);
      for (const [existing, handle] of watchers) {
        if (existing !== path && contains(path, existing)) { handle.close(); watchers.delete(existing); }
      }
    } catch {
      console.error(`Cannot watch ${path}; changes there require restarting dev.`);
    }
  }
  function launch() {
    const runtimeArguments = runtimeExportConditions().map(condition => `--conditions=${condition}`);
    const workerPath = Bun.fileURLToPath(new URL(import.meta.url.endsWith(".ts") ? "./dev-worker.ts" : "./dev-worker.js", import.meta.url));
    try {
      child = Bun.spawn([process.execPath, ...runtimeArguments, workerPath, config], {
        stdin: "ignore", stdout: "inherit", stderr: "inherit",
        ipc(message: unknown, sender) {
          if (stopped || child !== sender || !message || typeof message !== "object" || !("watch" in message)
            || !Array.isArray(message.watch)) return;
          const roots = [root, ...message.watch.filter((path): path is string => typeof path === "string").map(path => resolve(path))];
          const desired = roots.filter((path, index) => !roots.some((parent, other) =>
            other !== index && contains(parent, path) && (parent !== path || other < index)));
          for (const [path, watcher] of watchers) {
            if (!desired.includes(path)) { watcher.close(); watchers.delete(path); }
          }
          for (const path of desired) watchDirectory(path);
        },
      });
    } catch {
      console.error("Cannot start development worker; waiting for the next source change.");
    }
  }
  watchDirectory(root);
  if (!watchers.size) throw new Error("Cannot watch the project directory");
  launch();
  let closing: Promise<void> | undefined;
  return {
    closed,
    close() {
      closing ??= (async () => {
        stopped = true; revision++;
        clearTimeout(timer);
        for (const watcher of watchers.values()) watcher.close();
        watchers.clear();
        await chain;
        await stopChild();
        finish();
      })();
      return closing;
    },
  };
}
