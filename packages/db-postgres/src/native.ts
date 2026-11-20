import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

type Executable = "bridge" | "service" | "js";
const require = createRequire(import.meta.url);
// Literal resolutions keep executable files visible to server file tracing.
const assets = {
  "darwin-arm64": {
    bridge: () => require.resolve("@hulla/db-postgres-darwin-arm64/hulladb-bridge"),
    service: () => require.resolve("@hulla/db-postgres-darwin-arm64/hulladb-service"),
    js: () => require.resolve("@hulla/db-postgres-darwin-arm64/hulladb-js"),
  },
  "linux-x64": {
    bridge: () => require.resolve("@hulla/db-postgres-linux-x64/hulladb-bridge"),
    service: () => require.resolve("@hulla/db-postgres-linux-x64/hulladb-service"),
    js: () => require.resolve("@hulla/db-postgres-linux-x64/hulladb-js"),
  },
} as const;

export function resolveNativeExecutable(kind: Executable): string {
  const variable = { bridge: "HULLADB_ENGINE", service: "HULLADB_SERVICE", js: "HULLADB_JS" }[kind];
  const override = process.env[variable];
  if (override) return override;
  const platform = `${process.platform}-${process.arch}`;
  if (import.meta.url.endsWith("/src/native.ts")) {
    for (const path of [`../native/${platform}/hulladb-${kind}`, `../../../target/debug/hulladb-${kind}`]) {
      const development = new URL(path, import.meta.url);
      if (existsSync(development)) return fileURLToPath(development);
    }
  }
  const locate = assets[platform as keyof typeof assets]?.[kind];
  if (locate) {
    try {
      const path = locate();
      if (existsSync(path)) return path;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "MODULE_NOT_FOUND") throw error;
    }
  }
  throw new Error(`No packaged hulladb-${kind} executable for ${platform}. Install @hulla/db-postgres with optional dependencies on the deployment target, or set ${variable} to a compatible executable.`);
}
