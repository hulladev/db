import { fileURLToPath } from "node:url";
import { runtimeExportConditions } from "./runtime-conditions.ts";

/** Runs in the Node CLI before Bun is available, so this bootstrap uses Node APIs. */
export async function runDevelopmentCLI(): Promise<void> {
  const { spawn } = await import("node:child_process");
  const conditions = runtimeExportConditions().map(condition => `--conditions=${condition}`);
  const child = spawn("bun", [...conditions, fileURLToPath(new URL(import.meta.url.endsWith(".ts") ? "./cli.ts" : "./cli.js", import.meta.url)), ...process.argv.slice(2)], {
    stdio: "inherit",
  });
  const stop = () => { child.kill("SIGTERM"); };
  process.on("SIGINT", stop); process.on("SIGTERM", stop);
  try {
    process.exitCode = await new Promise<number>((resolve, reject) => {
      child.once("error", (error: NodeJS.ErrnoException) => reject(error.code === "ENOENT"
        ? new Error("This hulla-db command requires Bun on PATH. Install Bun and rerun the command.")
        : error));
      child.once("close", code => resolve(code ?? 1));
    });
  } finally { process.off("SIGINT", stop); process.off("SIGTERM", stop); }
}
