import { cp, mkdir } from "node:fs/promises";
import { join } from "node:path";
import { executables, verifyNative } from "../../packages/db/scripts/native-artifacts.ts";

export async function stagePostgresRuntime(source: string, destination: string, platform: string) {
  await mkdir(destination, { recursive: true });
  for (const executable of executables) {
    const path = join(source, platform, executable);
    await verifyNative(path, platform);
    await cp(path, join(destination, executable), { dereference: true });
  }
}
