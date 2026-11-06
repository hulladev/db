import { expect, test } from "bun:test";
import { mkdir, mkdtemp, rm, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { setupGeneratedIgnore } from "../src/setup-ignore.ts";
import { parseCLI } from "../src/cli-options.ts";

test("explicit ignore setup preserves project content, is idempotent, and does not generate or provision", async () => {
  const root = await mkdtemp(resolve(tmpdir(), "db-ignore-"));
  try {
    const config = resolve(root, "db.config.ts"), ignore = resolve(root, ".prettierignore");
    await Bun.write(config, `export default { schema: './data/schema.ts', database: { server: { url: 'postgresql://example:example@localhost/db' } } }`);
    const original = "# project-owned\r\nvendor/\r\n";
    await Bun.write(ignore, original);
    expect(await setupGeneratedIgnore(config, ignore)).toEqual({ path: ignore, pattern: "/data/_generated/", changed: true });
    const updated = await Bun.file(ignore).text();
    expect(updated.startsWith(original)).toBe(true);
    expect(updated).toContain("\r\n/data/_generated/\r\n");
    expect((await setupGeneratedIgnore(config, ignore)).changed).toBe(false);
    expect(await Bun.file(ignore).text()).toBe(updated);
    expect(await Bun.file(resolve(root, "data/_generated/server.ts")).exists()).toBe(false);
    await expect(setupGeneratedIgnore(config, resolve(root, "vite.config.ts"))).rejects.toThrow("gitignore syntax");
    await mkdir(resolve(root, "elsewhere"));
    await expect(setupGeneratedIgnore(config, resolve(root, "elsewhere/.ignore"))).rejects.toThrow("ancestor");
    const link = resolve(root, ".linkedignore");
    await symlink(ignore, link);
    await expect(setupGeneratedIgnore(config, link)).rejects.toThrow("regular file");
    expect(await Bun.file(ignore).text()).toBe(updated);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("setup requires an explicit ignore file and other operations cannot accept it silently", () => {
  expect(parseCLI(["setup", "--ignore-file", "../../.prettierignore"])).toEqual({ command: "setup", config: "db.config.ts", ignoreFile: "../../.prettierignore" });
  for (const args of [["setup"], ["setup", "--ignore-file"], ["generate", "--ignore-file", ".ignore"], ["setup", "--ignore-file", ".ignore", "--write"]]) expect(() => parseCLI(args)).toThrow();
});
