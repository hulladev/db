export const nativePlatforms = ["darwin-arm64", "darwin-x64", "linux-x64", "linux-arm64"] as const;
export const executables = ["hulladb-bridge", "hulladb-service", "hulladb-js"] as const;

/** Reject scripts, placeholders and accidentally staged host binaries. */
export async function verifyNative(path: string | URL, platform: string): Promise<void> {
  const file = Bun.file(path);
  if (((await file.stat()).mode & 0o111) === 0) throw new Error(`Native artifact is not executable: ${path}`);
  const header = new DataView(await file.slice(0, 64).arrayBuffer());
  const [os, arch] = platform.split("-");
  let valid = false;
  if (header.byteLength >= 64 && os === "linux") {
    valid = header.getUint32(0) === 0x7f454c46
      && header.getUint8(4) === 2 && header.getUint8(5) === 1 && header.getUint16(18, true) === (arch === "x64" ? 62 : 183);
  } else if (header.byteLength >= 32 && os === "darwin") {
    valid = header.getUint32(0, true) === 0xfeedfacf && header.getUint32(4, true) === (arch === "x64" ? 0x1000007 : 0x100000c);
  }
  if (!valid) throw new Error(`Native artifact is not a ${platform} executable: ${path}`);
}
