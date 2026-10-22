/** Shared Bun-only build command for the small framework packages. */
import { rm } from "node:fs/promises";

const root = process.cwd();
await rm(`${root}/dist`, { recursive: true, force: true });
const compiler = Bun.spawn([process.execPath, "x", "--no-install", "tsc", "-p", "tsconfig.build.json"], {
  cwd: root, stdout: "inherit", stderr: "inherit", timeout: 30_000, killSignal: "SIGKILL",
});
process.exitCode = await compiler.exited;
