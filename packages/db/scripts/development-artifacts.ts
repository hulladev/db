import { resolve } from "node:path";
import { verifyNative } from "./native-artifacts.ts";
export const developmentBun = "1.4.4";

export async function verifyDevelopment(root: string, platform: string): Promise<void> {
  for (const executable of ["hulladb-dev", "compiler/lib/tsc", "postgres/bin/postgres", "postgres/bin/initdb"]) {
    await verifyNative(resolve(root, "native", platform, executable), platform);
  }
  for (const file of ["development/compiler/dist/api/async/api.js", "development/compiler/LICENSE", "development/compiler/NOTICE.txt",
    `native/${platform}/postgres/COPYRIGHT`, `native/${platform}/postgres/build.json`, `native/${platform}/BUN-LICENSE.md`, `native/${platform}/compiler/NOTICE.txt`, `native/${platform}/compiler/lib/lib.d.ts`, `native/${platform}/development.json`]) {
    if (!await Bun.file(resolve(root, file)).exists()) throw new Error(`Missing development asset: ${file}. Run build:postgres and build:development on the target host.`);
  }
  const shared = await Bun.file(resolve(root, "development/compiler/package.json")).json();
  const native = await Bun.file(resolve(root, "native", platform, "compiler/package.json")).json();
  if (shared.version !== native.version) throw new Error(`Mismatched TypeScript development assets for ${platform}`);
  const build = await Bun.file(resolve(root, "native", platform, "development.json")).json();
  if (build.bun !== developmentBun || build.typescript !== shared.version) throw new Error(`Unexpected development tool versions for ${platform}`);
}
