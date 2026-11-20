/** Shared Bun-only build command for the small framework packages. */
import { chmod, rm } from "node:fs/promises";

const root = process.cwd();
await rm(`${root}/dist`, { recursive: true, force: true });
const compiler = Bun.spawn([process.execPath, "x", "--no-install", "tsc", "-p", "tsconfig.build.json"], {
  cwd: root, stdout: "inherit", stderr: "inherit", timeout: 30_000, killSignal: "SIGKILL",
});
const status = await compiler.exited;
if (status !== 0) process.exit(status);
const executable = process.argv[2];
if (executable) await chmod(`${root}/dist/${executable}`, 0o755);
