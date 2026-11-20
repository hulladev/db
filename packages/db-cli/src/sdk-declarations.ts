const sdk = new URL("../", import.meta.resolve("@hulla/db"));

export function sdkDeclarations(module: string): string[] {
  return [`src/${module}.ts`, `dist/${module}.d.ts`].map(path => Bun.fileURLToPath(new URL(path, sdk)));
}
