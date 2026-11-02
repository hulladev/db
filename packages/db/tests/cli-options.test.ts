import { expect, test } from "bun:test";
import { parseCLI } from "../src/cli-options.ts";

test("migration applies by default and check cannot enable other administration flags", () => {
  expect(parseCLI(["migrate"])).toEqual({ command: "migrate", config: "db.config.ts", check: false });
  expect(parseCLI(["migrate", "--check", "app/db.config.ts"])).toEqual({ command: "migrate", config: "app/db.config.ts", check: true });
  for (const flag of ["--studio", "--studio-port", "--write", "--read-only"]) expect(() => parseCLI(["migrate", flag])).toThrow();
});
