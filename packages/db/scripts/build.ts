import { chmod, rm } from "node:fs/promises";

const root = Bun.fileURLToPath(new URL("../", import.meta.url));
await rm(new URL("../dist", import.meta.url), { recursive: true, force: true });
const result = Bun.spawnSync([process.execPath, "x", "--no-install", "tsc", "-p", "tsconfig.build.json"], { cwd: root, stdout: "inherit", stderr: "inherit", timeout: 30_000, killSignal: "SIGKILL" });
if (result.exitCode !== 0) process.exit(result.exitCode);
await chmod(new URL("../dist/cli.js", import.meta.url), 0o755);
