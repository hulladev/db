import { expect, test } from "bun:test";
import { tmpdir } from "node:os";
import { createCLIOutput } from "../src/cli-output.ts";

function capture(isTTY: boolean, env: NodeJS.ProcessEnv) {
  let text = "";
  const output = createCLIOutput({ isTTY, write(value) { text += value; } }, env);
  return { output, text: () => text };
}

test("redirected and disabled terminal output preserves parseable readiness URLs without escapes", () => {
  for (const [isTTY, env] of [
    [false, { TERM_PROGRAM: "vscode", FORCE_COLOR: "1" }],
    [true, { TERM: "dumb", TERM_PROGRAM: "vscode" }],
    [true, { TERM_PROGRAM: "vscode", NO_COLOR: "1" }],
    [true, { TERM_PROGRAM: "vscode", FORCE_COLOR: "0" }],
  ] as const) {
    const { output, text } = capture(isTTY, env);
    output.listening("Database service", "http://127.0.0.1:3001");
    expect(text()).toBe("Database service listening at http://127.0.0.1:3001\n");
    output.heading("dev", "/app/db.config.ts");
    output.error("Cannot load config");
    output.help();
    expect(text()).not.toContain("\x1b");
  }
});

test("terminal links retain Studio fragments and encode local paths, with a visible URL fallback", () => {
  const url = "http://127.0.0.1:4983/#token=test-token";
  const { output, text } = capture(true, { TERM_PROGRAM: "vscode" });
  output.listening("Hulla Studio", url);
  output.info(output.file("/app/my project/db.config.ts"));
  expect(text()).toContain(`\x1b]8;;${url}\x1b\\`);
  expect(text()).toContain("\x1b]8;;file:///app/my%20project/db.config.ts\x1b\\");
  expect(text()).toContain("\x1b]8;;\x1b\\");
  const fallback = capture(true, { TERM: "xterm-256color", TERM_PROGRAM: "Apple_Terminal" });
  fallback.output.listening("Hulla Studio", url);
  expect(fallback.text()).toContain(url);
  expect(fallback.text()).not.toContain("\x1b]8;");
});

test("link labels and destinations cannot inject terminal control sequences", () => {
  const { output, text } = capture(true, { TERM_PROGRAM: "vscode" });
  output.info(output.file("/app/config.ts", "config\x1b]0;injected\x07\nlabel"));
  output.listening("Hulla Studio", "http://localhost:4983/\x1b]0;injected\x07");
  expect(text()).not.toContain("\x1b]0;");
  expect(text()).not.toContain("\x07");
  expect(text().split("\n")).toHaveLength(3);
});

test("CLI help runs without project or development assets on Node and Bun; invalid usage still fails", () => {
  const node = Bun.which("node");
  expect(node).not.toBeNull();
  const cli = Bun.fileURLToPath(new URL("../src/cli.ts", import.meta.url));
  for (const runtime of [process.execPath, node!]) {
    for (const args of [[], ["--help"], ["dev", "/missing/db.config.ts", "-h"], ["unknown-command"]]) {
      const child = Bun.spawnSync([runtime, cli, ...args], {
        cwd: tmpdir(), env: { ...process.env, FORCE_COLOR: "1" },
        stdout: "pipe", stderr: "pipe", timeout: 5000,
      });
      const invalid = args[0] === "unknown-command";
      expect(child.exitCode).toBe(invalid ? 1 : 0);
      const output = (invalid ? child.stderr : child.stdout).toString();
      expect(output).toContain("Usage: hulla-db");
      expect(output).toContain("--studio-port");
      expect(output).not.toContain("\x1b");
      expect((invalid ? child.stdout : child.stderr).toString()).toBe("");
    }
  }
});
