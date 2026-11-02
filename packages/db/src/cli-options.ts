export interface StudioFlags { readonly port: number; readonly readOnly: boolean }
export interface CLIOptions {
  readonly command: "generate" | "init" | "migrate" | "serve" | "dev" | "studio";
  readonly config: string;
  readonly studio?: StudioFlags;
  readonly check?: boolean;
}

export const usage = "Usage: hulla-db <generate|init|migrate|serve|dev|studio> [db.config.ts] [--check] [--studio] [--studio-port <port>] [--read-only|--write]";

export function parseCLI(args: readonly string[], production = false): CLIOptions {
  const [command, ...rest] = args;
  if (!["generate", "init", "migrate", "serve", "dev", "studio"].includes(command ?? "")) throw new Error(usage);
  let config: string | undefined, enabled = false, port: number | undefined, readOnly: boolean | undefined, check = false;
  for (let i = 0; i < rest.length; i++) {
    const arg = rest[i]!;
    if (arg === "--check" && command === "migrate" && !check) check = true;
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
  return {
    command: command as CLIOptions["command"], config: config ?? "db.config.ts",
    ...(command === "migrate" ? { check } : {}),
    ...(command === "studio" || enabled ? { studio: { port: port ?? 4983, readOnly: readOnly ?? production } } : {}),
  };
}
