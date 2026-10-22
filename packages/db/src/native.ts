import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

type Executable = "bridge" | "service" | "js";
// Literal URLs keep native assets visible to framework file tracing.
const assets = {
  "darwin-arm64": {
    bridge: new URL("../native/darwin-arm64/hulladb-bridge", import.meta.url),
    service: new URL("../native/darwin-arm64/hulladb-service", import.meta.url),
    js: new URL("../native/darwin-arm64/hulladb-js", import.meta.url),
  },
  "darwin-x64": {
    bridge: new URL("../native/darwin-x64/hulladb-bridge", import.meta.url),
    service: new URL("../native/darwin-x64/hulladb-service", import.meta.url),
    js: new URL("../native/darwin-x64/hulladb-js", import.meta.url),
  },
  "linux-x64": {
    bridge: new URL("../native/linux-x64/hulladb-bridge", import.meta.url),
    service: new URL("../native/linux-x64/hulladb-service", import.meta.url),
    js: new URL("../native/linux-x64/hulladb-js", import.meta.url),
  },
  "linux-arm64": {
    bridge: new URL("../native/linux-arm64/hulladb-bridge", import.meta.url),
    service: new URL("../native/linux-arm64/hulladb-service", import.meta.url),
    js: new URL("../native/linux-arm64/hulladb-js", import.meta.url),
  },
} as const;

export function resolveNativeExecutable(kind: Executable): string {
  const override = process.env[{ bridge: "HULLADB_ENGINE", service: "HULLADB_SERVICE", js: "HULLADB_JS" }[kind]];
  if (override) return override;
  const platform = `${process.platform}-${process.arch}`;
  const binaries = assets[platform as keyof typeof assets];
  if (binaries && existsSync(binaries[kind])) return fileURLToPath(binaries[kind]);
  // Only a source checkout can use developer-built binaries. Published modules
  // are always in dist and never consult the application's cwd or run Cargo.
  if (import.meta.url.endsWith("/src/native.ts")) {
    const development = new URL(`../../../target/debug/hulladb-${kind}`, import.meta.url);
    if (existsSync(development)) return fileURLToPath(development);
  }
  throw new Error(`No packaged hulladb-${kind} executable for ${platform}. Install an @hulla/db artifact containing native/${platform}, or set ${{ bridge: "HULLADB_ENGINE", service: "HULLADB_SERVICE", js: "HULLADB_JS" }[kind]} to a compatible executable.`);
}
