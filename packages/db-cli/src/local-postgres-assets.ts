import { chmod, mkdir, mkdtemp, open, readdir, rename, rm, stat, utimes } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { cliOutput } from "./cli-output.ts";

type Asset = { url: string; size: number; sha256: string };
const maxArchiveSize = 64 * 1024 * 1024;
const retainedArchives = 3;

function matches(bytes: Uint8Array, asset: Asset): boolean {
  return bytes.length === asset.size && new Bun.CryptoHasher("sha256").update(bytes).digest("hex") === asset.sha256;
}

async function download(asset: Asset, signal: AbortSignal): Promise<Uint8Array> {
  const response = await fetch(asset.url, { signal: AbortSignal.any([signal, AbortSignal.timeout(120_000)]) });
  if (!response.ok || !response.body) {
    await response.body?.cancel();
    throw new Error(`PostgreSQL download failed (HTTP ${response.status}). Supply a PostgreSQL URL or try again.`);
  }
  const bytes = new Uint8Array(asset.size);
  let length = 0;
  for await (const chunk of response.body) {
    if (length + chunk.length > bytes.length) throw new Error("PostgreSQL archive exceeds its expected size");
    bytes.set(chunk, length);
    length += chunk.length;
  }
  if (length !== bytes.length || !matches(bytes, asset)) throw new Error("PostgreSQL archive checksum mismatch");
  return bytes;
}

async function readCache(path: string, size: number): Promise<Uint8Array | undefined> {
  const file = await open(path, "r").catch((error: NodeJS.ErrnoException) => {
    if (error.code !== "ENOENT") throw error;
  });
  if (!file) return;
  try {
    const info = await file.stat();
    // Read through the opened descriptor so a parallel install can replace the cache.
    if (info.isFile() && info.size === size) return new Uint8Array(await Bun.file(file.fd).arrayBuffer());
  } finally { await file.close(); }
}

async function pruneCache(directory: string, platform: string, current: string) {
  const prefix = `${platform}-`;
  const candidates = await Promise.all((await readdir(directory))
    .filter(name => name.startsWith(prefix) && /^[a-f0-9]{64}\.tar\.gz$/.test(name.slice(prefix.length)))
    .map(async name => {
      const path = join(directory, name);
      return { path, time: await stat(path).then(info => info.mtimeMs, () => 0) };
    }));
  const stale = candidates.filter(file => file.path !== current).sort((a, b) => b.time - a.time);
  await Promise.all(stale.slice(retainedArchives - 1).map(file => rm(file.path, { force: true })));
}

/** Cache verified archives by digest. Each running server owns its extracted files. */
export async function openLocalPostgresAssets(signal: AbortSignal) {
  const platform = `${process.platform}-${process.arch}`;
  const manifest = await Bun.file(new URL("./local-postgres-assets.json", import.meta.url)).json().catch(error => {
    if (error?.code !== "ENOENT") throw error;
    throw new Error("This build has no local PostgreSQL download manifest. Supply a PostgreSQL URL.");
  }) as Record<string, Asset | undefined>;
  const asset = manifest[platform];
  if (!asset) throw new Error(`Managed local PostgreSQL is unavailable on ${platform}. Supply a PostgreSQL URL.`);
  if (!Number.isSafeInteger(asset.size) || asset.size <= 0 || asset.size > maxArchiveSize || !/^[a-f0-9]{64}$/.test(asset.sha256))
    throw new Error("Invalid local PostgreSQL download manifest");

  const cache = join(process.env.XDG_CACHE_HOME || join(homedir(), ".cache"), "hulla", "db", "postgres");
  const archive = join(cache, `${platform}-${asset.sha256}.tar.gz`);
  let bytes = await readCache(archive, asset.size);
  if (!bytes || !matches(bytes, asset)) {
    cliOutput.info(`Downloading local PostgreSQL for ${platform}…`);
    bytes = await download(asset, signal);
    signal.throwIfAborted();
    await mkdir(cache, { recursive: true, mode: 0o700 });
    const staging = await mkdtemp(join(cache, ".download-"));
    try {
      const file = join(staging, "archive.tar.gz");
      await Bun.write(file, bytes, { mode: 0o600 });
      await rename(file, archive);
    } finally { await rm(staging, { recursive: true, force: true }); }
    await pruneCache(cache, platform, archive).catch(() => cliOutput.warn("Could not prune the local PostgreSQL download cache."));
  } else {
    const now = new Date();
    await utimes(archive, now, now).catch(() => {});
  }

  signal.throwIfAborted();
  const directory = await mkdtemp(join(tmpdir(), "hulladb-postgres-runtime-"));
  const close = () => rm(directory, { recursive: true, force: true });
  try {
    await new Bun.Archive(bytes).extract(directory);
    const bin = join(directory, "bin");
    for (const name of ["postgres", "initdb"]) await chmod(join(bin, name), 0o755);
    signal.throwIfAborted();
    return { bin, close };
  } catch (error) {
    await close();
    throw error;
  }
}
