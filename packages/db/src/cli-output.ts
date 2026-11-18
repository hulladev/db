import { pathToFileURL } from "node:url";
import { usage } from "./cli-options.ts";

interface OutputStream { readonly isTTY?: boolean; write(text: string): unknown }

/** Shared by the Node bootstrap and Bun development workers. No runtime dependency. */
export function createCLIOutput(stream: OutputStream, env: NodeJS.ProcessEnv = process.env) {
  const interactive = !!stream.isTTY && env.TERM !== "dumb";
  const decorated = interactive && !env.NO_COLOR && env.FORCE_COLOR !== "0";
  const hyperlinks = decorated && (
    /^(iTerm\.app|WezTerm|vscode|ghostty)$/.test(env.TERM_PROGRAM ?? "")
    || /^(xterm-kitty|foot|foot-extra|wezterm)$/.test(env.TERM ?? "")
    || !!env.WT_SESSION || !!env.KONSOLE_VERSION || Number(env.VTE_VERSION) >= 5000
  );
  const style = (code: string, text: string) => decorated ? `\x1b[${code}m${text}\x1b[0m` : text;
  const line = (text: string) => { stream.write(text + "\n"); };
  const status = (symbol: string, color: string, message: string) =>
    line(decorated ? `  ${style(color, symbol)} ${message}` : message);
  const link = (url: string, label = url) => {
    // Never allow a path or URL to terminate an OSC sequence or inject terminal controls.
    const target = url.replace(/[\x00-\x1f\x7f-\x9f]/g, "");
    const text = label.replace(/[\x00-\x1f\x7f-\x9f]/g, "");
    return hyperlinks ? `\x1b]8;;${target}\x1b\\${style("36;4", text)}\x1b]8;;\x1b\\` : style("36;4", text);
  };
  return {
    success: (message: string) => status("✓", "32", message),
    info: (message: string) => status("›", "2", message),
    warn: (message: string) => status("!", "33", message),
    error: (message: string) => status("✗", "31", message),
    file: (path: string, label = path) => link(pathToFileURL(path).href, label),
    heading(command: string, configPath: string) {
      if (!interactive) return;
      line(`\n  ${style("1;36", "hulla/db")} ${style("2", command)}`);
      line(`  ${style("2", "Source")}  ${link(pathToFileURL(configPath).href, configPath)}\n`);
    },
    listening(label: string, url: string) {
      status("✓", "32", `${style("1", label)} listening at ${link(url)}`);
    },
    help() {
      line(`\n${style("1;36", "hulla/db")} — typed database tools\n`);
      line(usage);
      line(`\n${style("1", "Commands")}`);
      for (const [command, description] of [
        ["dev", "Generate, start the service, and watch for changes"],
        ["generate", "Generate typed database and function entrypoints"],
        ["setup", "Add generated output to an explicitly selected ignore file"],
        ["init", "Initialize the database and verify its schema"],
        ["migrate", "Apply supported schema changes; use --check to preview"],
        ["serve", "Start the service from generated artifacts"],
        ["studio", "Open the database administration UI"],
      ] as const) line(`  ${style("36", command.padEnd(12))}${description}`);
      line(`\n${style("1", "Options")}`);
      for (const [option, description] of [
        ["entrypoint or config", "Generate from db/server.ts, or use db.config.ts (default)"],
        ["--check", "Preview schema changes without applying them"],
        ["--ignore-file <path>", "Setup only: gitignore-syntax file used by your tools"],
        ["--studio", "Start Studio alongside dev"],
        ["--studio-port <port>", "Studio port (default: 4983; 0 picks a free port)"],
        ["--read-only", "Disable writes in Studio (default in production)"],
        ["--write", "Allow writes in Studio"],
        ["-h, --help", "Show this help"],
      ] as const) line(`  ${style("36", option.padEnd(24))}${description}`);
      line(`\n${style("1", "Examples")}`);
      line("  hulla-db dev --studio");
      line("  hulla-db studio --read-only");
      line("  hulla-db setup --ignore-file '<ignore-file-path>'");
      line("  hulla-db generate ./db/server.ts\n");
    },
  };
}

export const cliOutput = createCLIOutput(process.stdout);
export const cliError = createCLIOutput(process.stderr);
