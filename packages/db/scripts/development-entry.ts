// Keep project imports on the real filesystem. Bundling their SDK copies would
// split declaration brands and break discovery across the compiler boundary.
import { dirname, resolve } from "node:path";
const cli = resolve(dirname(process.execPath), "../../dist/cli.js");
const child = Bun.spawn([process.execPath, cli, ...process.argv.slice(2)], {
  env: { ...process.env, BUN_BE_BUN: "1" }, stdin: "inherit", stdout: "inherit", stderr: "inherit",
});
const stop = () => child.kill("SIGTERM");
process.on("SIGINT", stop); process.on("SIGTERM", stop);
try { process.exitCode = await child.exited; }
finally { process.off("SIGINT", stop); process.off("SIGTERM", stop); }
