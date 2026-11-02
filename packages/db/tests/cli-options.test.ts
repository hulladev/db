import { expect, test } from "bun:test";
import { parseCLI } from "../src/cli-options.ts";

test("Studio flags are explicit and production access defaults to read-only", () => {
  expect(parseCLI(["dev", "app/db.config.ts", "--studio", "--studio-port", "0"])).toEqual({
    command: "dev", config: "app/db.config.ts", studio: { port: 0, readOnly: false },
  });
  expect(parseCLI(["studio"], true).studio).toEqual({ port: 4983, readOnly: true });
  expect(parseCLI(["studio", "--write"], true).studio?.readOnly).toBe(false);
  expect(parseCLI(["dev", "--studio", "--read-only"]).studio?.readOnly).toBe(true);
  expect(parseCLI(["serve"])).toEqual({ command: "serve", config: "db.config.ts" });
});

test("unsupported combinations cannot silently enable administration or ignore access settings", () => {
  for (const args of [
    ["serve", "--studio"], ["generate", "--write"], ["dev", "--studio-port", "4000"],
    ["studio", "--read-only", "--write"], ["studio", "--studio"],
    ["studio", "--studio-port", "65536"], ["studio", "--studio-port", ""],
    ["studio", "--studio-port", "-1"], ["studio", "--studio-port", "NaN"],
    ["dev", "config.ts", "extra.ts"],
    ["init", "--check"], ["dev", "--check"], ["serve", "--check"],
    ["migrate", "--apply"], ["migrate", "--check", "--check"],
  ]) expect(() => parseCLI(args)).toThrow();
});

test("migration applies by default and check cannot enable other administration flags", () => {
  expect(parseCLI(["migrate"])).toEqual({ command: "migrate", config: "db.config.ts", check: false });
  expect(parseCLI(["migrate", "--check", "app/db.config.ts"])).toEqual({ command: "migrate", config: "app/db.config.ts", check: true });
  for (const flag of ["--studio", "--studio-port", "--write", "--read-only"]) expect(() => parseCLI(["migrate", flag])).toThrow();
});
