import { expect, test } from "bun:test";
import { defineSchema, defineTable, v } from "@hulla/db";
import { expoSQLiteDriver, sqlite } from "@hulla/db-sqlite/expo";

const schema = defineSchema({ tasks: defineTable({ title: v.string() }) });

test("the Expo entrypoint bundles without Node or Bun drivers", async () => {
  const build = await Bun.build({
    entrypoints: [new URL("../src/expo.ts", import.meta.url).pathname],
    target: "browser",
    conditions: ["hulladb-source"],
    external: ["expo-crypto", "expo-sqlite"],
    metafile: true,
  });
  expect(build.success).toBe(true);
  expect(Object.keys(build.metafile!.inputs).some(path => /(?:node|bun)-driver/.test(path))).toBe(false);
  const output = (await Promise.all(build.outputs.map(item => item.text()))).join("\n");
  expect(output).not.toContain("node:sqlite");
  expect(output).not.toContain("bun:sqlite");
});

test("filename setup reopens an existing journal and uses Expo entropy without Web Crypto", async () => {
  const path = new URL("./fixtures/expo-filename.ts", import.meta.url).pathname;
  const child = Bun.spawn([process.execPath, "--conditions=hulladb-source", path], { stdout: "pipe", stderr: "pipe", timeout: 15_000 });
  const [exit, stdout, stderr] = await Promise.all([child.exited, new Response(child.stdout).text(), new Response(child.stderr).text()]);
  expect({ exit, stdout, stderr }).toEqual({ exit: 0, stdout: "", stderr: "" });
});

test("persistent Expo locks close both failed connections and surface through readiness", async () => {
  const busy = Object.assign(new Error("Calling getAllAsync failed\n→ Caused by: Error code 5: database is locked"), {
    code: "ERR_INTERNAL_SQLITE_ERROR",
  });
  let opens = 0, closes = 0;
  const db = sqlite({ schema, open() {
    opens++;
    return expoSQLiteDriver({
      async getAllAsync() { throw busy; },
      async closeAsync() { closes++; },
    });
  } });
  try {
    await expect(db.ready()).rejects.toBe(busy);
    expect(db.state).toEqual({ status: "error", error: busy });
    expect([opens, closes]).toEqual([2, 2]);
  } finally { await db.close(); }
});
