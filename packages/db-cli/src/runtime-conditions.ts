/** Keep spawned workers and bundled declarations on the parent's package exports. */
export function runtimeExportConditions(): string[] {
  return process.execArgv.flatMap((argument, index, args) => {
    if (argument.startsWith("--conditions=")) return [argument.slice("--conditions=".length)];
    if ((argument === "--conditions" || argument === "-C") && args[index + 1]) return [args[index + 1]!];
    return [];
  });
}
