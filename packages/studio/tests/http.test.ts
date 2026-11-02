import { expect, test } from "bun:test";
import { fileURLToPath } from "node:url";

for (const executable of [process.env.NODE ?? "node", process.execPath]) {
  test(`Studio authenticates and bounds its operator surface under ${executable}`, async () => {
    // Resolve version-manager shims so the timeout owns the listening process.
    const runtime = Bun.spawnSync([executable, "-p", "process.execPath"], { stdout: "pipe", stderr: "pipe" });
    expect(runtime.exitCode).toBe(0);
    const child = Bun.spawn([runtime.stdout.toString().trim(), "--conditions=hulladb-source", fileURLToPath(new URL("./http.integration.ts", import.meta.url))], { stdin: "ignore", stdout: "pipe", stderr: "pipe" });
    const drained = Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text()]);
    const timeout = setTimeout(() => child.kill("SIGKILL"), 15_000);
    try {
      const code = await child.exited;
      const output = (await drained).join("");
      expect(output).toContain("Studio HTTP access and lifecycle checks passed");
      expect(code).toBe(0);
    } finally {
      clearTimeout(timeout);
      if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
      await child.exited; await drained;
    }
  }, 17_000);
}
