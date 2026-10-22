import { spawn } from "node:child_process";

/** Exercise deployment commands in the fixture's runtime; builds require Bun. */
export async function runCommand(executable: string, args: readonly string[]): Promise<{ code: number | null; stdout: string; stderr: string }> {
  const child = spawn(executable, ["--conditions=hulladb-source", ...args], { stdio: ["ignore", "pipe", "pipe"] });
  let stdout = "", stderr = "";
  child.stdout.on("data", chunk => { stdout += String(chunk); });
  child.stderr.on("data", chunk => { stderr += String(chunk); });
  const code = await new Promise<number | null>((resolve, reject) => {
    child.once("error", reject);
    child.once("close", resolve);
  });
  return { code, stdout, stderr };
}
