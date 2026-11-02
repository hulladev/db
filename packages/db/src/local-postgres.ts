import { chmod, lstat, mkdir, open, rename, rm } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { cliError, cliOutput } from "./cli-output.ts";

export interface LocalPostgres { readonly url: string; readonly closed: Promise<void>; close(): Promise<void> }

function unusedPort(): number {
  const listener = Bun.listen({ hostname: "127.0.0.1", port: 0, socket: { data() {} } });
  const port = listener.port;
  listener.stop(true);
  return port;
}

function signalGroup(child: Bun.Subprocess, signal: NodeJS.Signals) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  try { process.kill(-child.pid, signal); } catch (error) { if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error; }
}

async function saveDiagnostics(stream: ReadableStream<Uint8Array>, path: string) {
  let tail = Buffer.alloc(0);
  for await (const chunk of stream) tail = Buffer.concat([tail, chunk]).subarray(-16 * 1024);
  try { await Bun.write(path, tail, { mode: 0o600, createPath: false }); }
  catch { cliError.warn("Cannot save local PostgreSQL diagnostics to .hulla/postgres.log"); }
}

/** Own a foreground postmaster. No pg_ctl daemon and no host PostgreSQL lookup. */
export async function startLocalPostgres(configPath: string, signal: AbortSignal): Promise<LocalPostgres> {
  const root = resolve(dirname(configPath), ".hulla");
  const binaries = Bun.fileURLToPath(new URL(`../native/${process.platform}-${process.arch}/postgres/bin/`, import.meta.url));
  const environment = { ...process.env, LD_LIBRARY_PATH: resolve(binaries, "../lib") };
  for (const name of ["postgres", "initdb"]) {
    if (!await Bun.file(resolve(binaries, name)).exists()) throw new Error(`Local PostgreSQL is missing for ${process.platform}-${process.arch}. Install matching @hulla/db development assets or provide DATABASE_URL.`);
  }
  await mkdir(root, { recursive: true, mode: 0o700 });
  const info = await lstat(root);
  if (!info.isDirectory() || info.isSymbolicLink()) throw new Error("Local database directory must be a real directory: .hulla");
  await chmod(root, 0o700);
  const lock = resolve(root, "dev.lock");
  try { await mkdir(lock, { mode: 0o700 }); }
  catch { throw new Error("Local database is locked. Stop the other hulla-db dev process. After a crash, confirm its processes have exited before removing .hulla/dev.lock."); }
  let child: Bun.Subprocess | undefined;
  let closing: Promise<void> | undefined;
  let initializing: string | undefined;
  let draining: Promise<void> | undefined;
  const close = () => closing ??= (async () => {
    if (child) {
      signalGroup(child, "SIGINT");
      const immediate = setTimeout(() => signalGroup(child!, "SIGQUIT"), 8000);
      const force = setTimeout(() => signalGroup(child!, "SIGKILL"), 12_000);
      try { await child.exited; } finally { clearTimeout(immediate); clearTimeout(force); }
      await draining;
    }
    if (initializing) await rm(initializing, { recursive: true, force: true });
    await rm(lock, { recursive: true });
  })();
  const abort = () => { if (child) signalGroup(child, "SIGINT"); };
  signal.addEventListener("abort", abort);
  try {
    await Bun.write(resolve(lock, "owner.json"), JSON.stringify({ pid: process.pid }), { mode: 0o600, createPath: false });
    signal.throwIfAborted();
    const state = resolve(root, "local.json"), data = resolve(root, "postgres");
    const stateInfo = await lstat(state).catch((error: NodeJS.ErrnoException) => { if (error.code !== "ENOENT") throw error; });
    if (stateInfo && (!stateInfo.isFile() || stateInfo.isSymbolicLink())) throw new Error(".hulla/local.json must be a regular file.");
    let url: string;
    try { url = (await Bun.file(state).json()).url; }
    catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw new Error("Cannot read .hulla/local.json. Preserve the data and repair the file, or explicitly recreate development storage.");
      if (await Bun.file(resolve(data, "PG_VERSION")).exists()) throw new Error("Local database credentials are missing. Preserve .hulla/postgres and restore .hulla/local.json; development never resets existing storage.");
      url = `postgresql://hulla:${crypto.randomUUID().replaceAll("-", "")}@127.0.0.1:${unusedPort()}/postgres`;
      // Bun.write has no exclusive-create flag. Open once with wx to protect
      // existing credentials, then let Bun write through the owned descriptor.
      const file = await open(state, "wx", 0o600);
      try { await Bun.write(Bun.file(file.fd), JSON.stringify({ url }) + "\n"); }
      finally { await file.close(); }
    }
    await chmod(state, 0o600);
    const parsed = new URL(url);
    if (parsed.protocol !== "postgresql:" || parsed.hostname !== "127.0.0.1" || parsed.username !== "hulla"
      || !parsed.password || !parsed.port || parsed.pathname !== "/postgres" || parsed.search || parsed.hash)
      throw new Error("Invalid managed connection in .hulla/local.json. Restore it or explicitly recreate development storage.");
    const dataInfo = await lstat(data).catch((error: NodeJS.ErrnoException) => { if (error.code !== "ENOENT") throw error; });
    if (dataInfo && (!dataInfo.isDirectory() || dataInfo.isSymbolicLink())) throw new Error(".hulla/postgres must be a real PostgreSQL data directory.");
    if (!dataInfo) {
      initializing = resolve(root, `initializing-${crypto.randomUUID()}`);
      child = Bun.spawn([resolve(binaries, "initdb"), "-D", initializing, "-U", "hulla", "--encoding=UTF8", "--locale=C", "--auth=scram-sha-256", "--pwfile=/dev/stdin"], {
        detached: true, env: environment, stdin: new TextEncoder().encode(parsed.password + "\n"), stdout: "ignore", stderr: "pipe",
      });
      // initdb starts a bootstrap postmaster; cancellation addresses its entire group.
      const timeout = setTimeout(() => signalGroup(child!, "SIGKILL"), 30_000);
      const diagnostics = saveDiagnostics(child.stderr as ReadableStream<Uint8Array>, resolve(root, "postgres.log"));
      let code: number;
      try { code = await child.exited; await diagnostics; } finally { clearTimeout(timeout); }
      child = undefined;
      signal.throwIfAborted();
      if (code !== 0) throw new Error("Cannot initialize local PostgreSQL. See .hulla/postgres.log; run as an ordinary user with available disk space.");
      await rename(initializing, data); initializing = undefined;
    } else if (!await Bun.file(resolve(data, "PG_VERSION")).exists()) {
      throw new Error("Existing .hulla/postgres is incomplete. Preserve or explicitly remove it before restarting; development never resets data.");
    }
    signal.throwIfAborted();
    child = Bun.spawn([resolve(binaries, "postgres"), "-D", data, "-h", "127.0.0.1", "-p", parsed.port,
      "-c", "unix_socket_directories=", "-c", "wal_level=logical", "-c", "max_slot_wal_keep_size=256MB"], {
      detached: true, env: environment, stdin: "ignore", stdout: "ignore", stderr: "pipe",
    });
    // Drain continuously without retaining an unbounded development log.
    draining = saveDiagnostics(child.stderr as ReadableStream<Uint8Array>, resolve(root, "postgres.log"));
    const postmaster = child;
    const closed = postmaster.exited.then(async () => {
      await draining;
      if (!closing) throw new Error("Local PostgreSQL stopped unexpectedly. See .hulla/postgres.log and restart development.");
    });
    void closed.catch(() => {});
    const deadline = Date.now() + 20_000;
    while (true) {
      signal.throwIfAborted();
      if (postmaster.exitCode !== null || postmaster.signalCode !== null || Date.now() > deadline)
        throw new Error("Local PostgreSQL did not become ready. Check its saved port is available and the data matches the bundled PostgreSQL major version; existing data was preserved.");
      const sql = new Bun.SQL({ url, max: 1, connectionTimeout: 1 });
      try { await sql`SELECT 1`; break; }
      catch { await Bun.sleep(50); }
      finally { await sql.close({ timeout: 0 }); }
    }
    cliOutput.success(`Local PostgreSQL listening at 127.0.0.1:${parsed.port} (data: ${cliOutput.file(data, ".hulla/postgres")})`);
    return { url, closed, close: async () => { signal.removeEventListener("abort", abort); await close(); } };
  } catch (error) {
    signal.removeEventListener("abort", abort);
    await close();
    throw error;
  }
}
