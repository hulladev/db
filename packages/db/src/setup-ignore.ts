import { lstat, mkdir } from "node:fs/promises";
import { dirname, isAbsolute, relative, resolve, sep } from "node:path";
import { defineConfig, type DatabaseConfig } from "./config.ts";

/** Explicit setup for a user-selected gitignore-syntax file; never detects a formatter. */
export async function setupGeneratedIgnore(configPath: string, ignoreFile: string): Promise<{ path: string; pattern: string; changed: boolean }> {
  const source = resolve(configPath);
  const config = defineConfig((await import(Bun.pathToFileURL(source).href)).default as DatabaseConfig);
  const output = resolve(dirname(resolve(dirname(source), config.schema ?? "db/schema.ts")), "_generated");
  const path = resolve(ignoreFile);
  if (/\.(?:jsonc?|[cm]?[jt]s)$/i.test(path)) throw new Error("--ignore-file expects gitignore syntax, not a JSON or source-code configuration file.");
  const fromIgnore = relative(dirname(path), output);
  if (!fromIgnore || fromIgnore === ".." || fromIgnore.startsWith(`..${sep}`) || isAbsolute(fromIgnore)) throw new Error("Choose an ignore file in an ancestor of the generated output directory.");
  const info = await lstat(path).catch((error: NodeJS.ErrnoException) => { if (error.code === "ENOENT") return undefined; throw error; });
  if (info && (!info.isFile() || info.isSymbolicLink())) throw new Error(`Ignore path must be a regular file: ${path}`);
  const pattern = "/" + fromIgnore.split(sep).join("/").replace(/[\\*?\[\]]/g, "\\$&") + "/";
  const before = info ? await Bun.file(path).text() : "";
  if (before.split(/\r?\n/).some(line => line.trim() === pattern)) return { path, pattern, changed: false };
  const newline = before.includes("\r\n") ? "\r\n" : "\n";
  await mkdir(dirname(path), { recursive: true });
  await Bun.write(path, before + (before && !before.endsWith("\n") ? newline : "") + `# Managed database output (@hulla/db)${newline}${pattern}${newline}`);
  return { path, pattern, changed: true };
}
