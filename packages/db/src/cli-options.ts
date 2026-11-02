export interface CLIOptions {
  readonly command: "generate" | "init" | "migrate" | "serve" | "dev";
  readonly config: string;
  readonly check?: boolean;
}

export const usage = "Usage: hulla-db <generate|init|migrate|serve|dev> [db.config.ts] [--check]";

export function parseCLI(args: readonly string[]): CLIOptions {
  const [command, ...rest] = args;
  if (!["generate", "init", "migrate", "serve", "dev"].includes(command ?? "")) throw new Error(usage);
  let config: string | undefined, check = false;
  for (const arg of rest) {
    if (arg === "--check" && command === "migrate" && !check) check = true;
    else if (!arg.startsWith("-") && config === undefined) config = arg;
    else throw new Error(usage);
  }
  return { command: command as CLIOptions["command"], config: config ?? "db.config.ts", ...(command === "migrate" ? { check } : {}) };
}
