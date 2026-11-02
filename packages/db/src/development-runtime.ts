import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { runtimeExportConditions } from "./runtime-conditions.ts";

/** Runs in the Node CLI before Bun is available, so this bootstrap uses Node APIs. */
export function developmentRuntime(): string {
  const runtime = new URL(`../native/${process.platform}-${process.arch}/hulladb-dev`, import.meta.url);
  if (existsSync(runtime)) return fileURLToPath(runtime);
  if (import.meta.url.endsWith("/src/development-runtime.ts") && typeof Bun !== "undefined") return process.execPath;
  throw new Error(`No development tools for ${process.platform}-${process.arch}. Install an @hulla/db package containing the matching native development assets.`);
}

export async function runDevelopmentCLI(): Promise<void> {
  const { spawn } = await import("node:child_process");
  const conditions = runtimeExportConditions().map(condition => `--conditions=${condition}`);
  const child = spawn(developmentRuntime(), [...conditions, fileURLToPath(new URL(import.meta.url.endsWith(".ts") ? "./cli.ts" : "./cli.js", import.meta.url)), ...process.argv.slice(2)], {
    stdio: "inherit", env: { ...process.env, BUN_BE_BUN: "1" },
  });
  const stop = () => { child.kill("SIGTERM"); };
  process.on("SIGINT", stop); process.on("SIGTERM", stop);
  try {
    process.exitCode = await new Promise<number>((resolve, reject) => {
      child.once("error", () => reject(new Error("Cannot start the packaged development runtime")));
      child.once("close", code => resolve(code ?? 1));
    });
  } finally { process.off("SIGINT", stop); process.off("SIGTERM", stop); }
}
