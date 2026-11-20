const identifier = /^[A-Za-z_$][A-Za-z0-9_$]*$/;
const reserved = new Set(
  "default class function var const let import export return new delete await yield enum implements interface package private protected public static null true false if else for while do switch case break continue throw try catch finally extends super this void typeof in instanceof with debugger".split(
    " ",
  ),
);

export function validExportName(name: string): boolean {
  return identifier.test(name) && !reserved.has(name);
}

export function assertModuleName(name: string): void {
  // Module identities are relative file paths, not JavaScript identifiers.
  if (!name.split("/").every(segment => /^[A-Za-z0-9_$-]+$/.test(segment)) || name === "runtime")
    throw new Error(`Invalid function module name: ${name}`);
}
