export interface StudioFlags { readonly port: number; readonly readOnly: boolean }
export interface CLIOptions {
  readonly command: "setup" | "generate" | "init" | "migrate" | "serve" | "dev" | "studio";
  readonly ignoreFile?: string;
  readonly config: string;
  readonly studio?: StudioFlags;
  readonly check?: boolean;
}

export const usage = "Usage: hulla-db <setup|generate|init|migrate|serve|dev|studio> [entrypoint-or-db.config.ts] [--ignore-file <path>] [--check] [--studio] [--studio-port <port>] [--read-only|--write]";

export function parseCLI(args: readonly string[], production = false): CLIOptions {
  const [command, ...rest] = args;
  if (!["setup", "generate", "init", "migrate", "serve", "dev", "studio"].includes(command ?? "")) throw new Error(usage);
  let config: string | undefined, enabled = false, port: number | undefined, readOnly: boolean | undefined, check = false;
  let ignoreFile: string | undefined;
  for (let i = 0; i < rest.length; i++) {
    const arg = rest[i]!;
    if (arg === "--ignore-file" && command === "setup" && ignoreFile === undefined) {
      const value = rest[++i];
      if (!value || value.startsWith("-") || /[\r\n\0]/.test(value)) throw new Error("--ignore-file requires an explicit gitignore-syntax file path");
      ignoreFile = value;
    }
    else if (arg === "--check" && command === "migrate" && !check) check = true;
    else if (arg === "--studio" && !enabled) enabled = true;
    else if (arg === "--studio-port" && port === undefined) {
      const value = rest[++i];
      if (!value || !/^\d+$/.test(value) || Number(value) > 65535) throw new Error("--studio-port requires an integer from 0 to 65535");
      port = Number(value);
    } else if ((arg === "--read-only" || arg === "--write") && readOnly === undefined) readOnly = arg === "--read-only";
    else if (!arg.startsWith("-") && config === undefined) config = arg;
    else throw new Error(usage);
  }
  if ((enabled && command !== "dev") || ((port !== undefined || readOnly !== undefined) && command !== "studio" && !(command === "dev" && enabled))) throw new Error(usage);
  if (command === "setup" && ignoreFile === undefined) throw new Error("setup requires --ignore-file <path>; choose the ignore file your tools use.");
  return {
    command: command as CLIOptions["command"], config: config ?? "db.config.ts",
    ...(ignoreFile === undefined ? {} : { ignoreFile }),
    ...(command === "migrate" ? { check } : {}),
    ...(command === "studio" || enabled ? { studio: { port: port ?? 4983, readOnly: readOnly ?? production } } : {}),
  };
}
