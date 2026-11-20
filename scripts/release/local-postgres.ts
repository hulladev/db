import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { verifyNative } from "../../packages/db/scripts/native-artifacts.ts";
import build from "../../packages/db/scripts/postgres-build.json" with { type: "json" };

export const postgresAssetTag = `postgresql-${build.version}-r${build.revision}`;
export const postgresAssetOrigin = `https://github.com/hulladev/db/releases/download/${postgresAssetTag}/`;
export const postgresManifestName = "local-postgres-assets.json";

type Asset = { url: string; size: number; sha256: string };
type Manifest = Record<string, Asset>;

export function postgresArchiveName(platform: string) {
  return `hulladb-postgres-${platform}.tar.gz`;
}

function describeArchive(bytes: ArrayBuffer, url: string): Asset {
  return { url, size: bytes.byteLength, sha256: new Bun.CryptoHasher("sha256").update(bytes).digest("hex") };
}

export async function preparePostgresArchives(source: string, output: string, platforms: readonly string[], origin: string) {
  await mkdir(output, { recursive: true });
  const manifest: Manifest = {};
  for (const platform of platforms) {
    const directory = join(source, platform, "postgres");
    for (const executable of ["bin/postgres", "bin/initdb"]) await verifyNative(join(directory, executable), platform);
    for (const file of ["COPYRIGHT", "build.json", "share/postgres.bki"])
      if (!await Bun.file(join(directory, file)).exists()) throw new Error(`Missing ${platform} PostgreSQL asset: ${file}`);
    const metadata = await Bun.file(join(directory, "build.json")).json();
    if (metadata.version !== build.version || metadata.sha256 !== build.sha256)
      throw new Error(`Unexpected PostgreSQL source build for ${platform}`);

    const name = postgresArchiveName(platform);
    const archive = join(output, name);
    const result = Bun.spawnSync(["tar", "-czf", archive, "-C", directory, "bin", "lib", "share", "COPYRIGHT", "build.json"], {
      env: { ...process.env, COPYFILE_DISABLE: "1" }, stderr: "pipe",
    });
    if (result.exitCode !== 0) throw new Error(`Cannot archive PostgreSQL for ${platform}: ${result.stderr}`);
    manifest[platform] = describeArchive(await Bun.file(archive).arrayBuffer(), new URL(name, origin).href);
  }
  return manifest;
}

export async function reusePostgresArchives(output: string, platforms: readonly string[], origin = postgresAssetOrigin) {
  const response = await fetch(new URL(postgresManifestName, origin), { signal: AbortSignal.timeout(30_000) });
  if (!response.ok) throw new Error(`Cannot read the published PostgreSQL manifest (HTTP ${response.status})`);
  const manifest = await response.json() as Manifest;
  await mkdir(output, { recursive: true });
  for (const platform of platforms) {
    const name = postgresArchiveName(platform);
    const expected = manifest[platform];
    const url = new URL(name, origin).href;
    if (expected?.url !== url || !Number.isSafeInteger(expected.size) || expected.size <= 0 ||
        expected.size > 64 * 1024 * 1024 || !/^[a-f0-9]{64}$/.test(expected.sha256))
      throw new Error(`Invalid published PostgreSQL manifest entry for ${platform}`);
    const archive = await fetch(url, { signal: AbortSignal.timeout(120_000) });
    if (!archive.ok) throw new Error(`Cannot download published ${platform} PostgreSQL (HTTP ${archive.status})`);
    const bytes = await archive.arrayBuffer();
    const actual = describeArchive(bytes, url);
    if (actual.size !== expected.size || actual.sha256 !== expected.sha256)
      throw new Error(`Published PostgreSQL archive differs from the manifest for ${platform}`);
    await Bun.write(join(output, name), bytes);
  }
  return manifest;
}
