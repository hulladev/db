import { expect, test } from "bun:test";
import { defineConfig } from "../src/config.ts";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { startService, verifyServiceIdentity } from "../src/service.ts";

test("verified identity pins are stable across object order and change with claims or account", async () => {
  const headers = new Headers({ authorization: "Bearer app-credential" });
  const first = await verifyServiceIdentity(received => {
    expect(received.get("authorization")).toBe("Bearer app-credential");
    return { key: "alice", identity: { role: "member", user: "alice" } };
  }, headers);
  const reordered = await verifyServiceIdentity(() => ({ key: "alice", identity: { user: "alice", role: "member" } }), headers);
  expect(reordered).toEqual(first);
  const changed = await verifyServiceIdentity(() => ({ key: "alice", identity: { user: "alice", role: "admin" } }), headers);
  expect(changed.principal).toBe(first.principal);
  expect(changed.authorizationScope).not.toBe(first.authorizationScope);
  const other = await verifyServiceIdentity(() => ({ key: "bob", identity: first.identity }), headers);
  expect(other.principal).not.toBe(first.principal);
  const anonymous = await verifyServiceIdentity(undefined, new Headers({ "x-hulla-principal": first.principal }));
  expect(anonymous.identity).toBeNull();
  expect(anonymous.principal).not.toBe(first.principal);
});

test("invalid verifier results fail instead of establishing anonymous or transformed identity", async () => {
  for (const result of [undefined, {}, { key: "", identity: {} }, { key: "alice", identity: undefined }, { key: "alice", identity: { expires: new Date() } }, { key: "alice", identity: { count: NaN } }]) {
    await expect(verifyServiceIdentity(() => result as never, new Headers())).rejects.toThrow();
  }
  await expect(verifyServiceIdentity(() => { throw new Error("expired"); }, new Headers())).rejects.toThrow("expired");
});

test("service configuration validates origins and snapshots them without starting a listener", () => {
  const origins = ["https://app.example.test"];
  const config = defineConfig({ database: { server: { url: "postgresql://localhost/db" } }, service: { port: 0, origins } });
  origins.push("https://different.example.test");
  expect(config.service?.origins).toEqual(["https://app.example.test"]);
  for (const service of [{ port: -1 }, { port: null }, { origins: ["*"] }, { origins: ["https://app.example.test/path"] }, { host: "host/route" }]) {
    expect(() => defineConfig({ database: config.database, service: service as never })).toThrow();
  }
});

test("a stalled provider verification expires and later credentials can still be verified", async () => {
  await expect(verifyServiceIdentity(() => new Promise(() => {}), new Headers())).rejects.toThrow("timed out");
  const recovered = await verifyServiceIdentity(() => ({ key: "alice", identity: { id: "alice" } }), new Headers());
  expect(recovered.identity).toEqual({ id: "alice" });
}, 8000);

// A production start must neither repair missing build artifacts nor initialize storage.
test("service startup requires readable generated artifacts without generating them", async () => {
  const root = await mkdtemp(new URL("./service-artifacts-", import.meta.url).pathname);
  try {
    await mkdir(resolve(root, "db"));
    const config = resolve(root, "db.config.ts");
    await writeFile(config, `export default { database: { server: { url: 'postgresql://localhost:1/unused' } } };`);
    await writeFile(resolve(root, "db/schema.ts"), `import { defineSchema, defineTable, v } from '@hulla/db/schema'; export default defineSchema({ tasks: defineTable({ title: v.string() }) });`);
    await expect(startService(config)).rejects.toThrow("run hulla-db generate with Bun before serve");
    await expect(readFile(resolve(root, "db/_generated/worker.js"))).rejects.toThrow();
    await mkdir(resolve(root, "db/_generated/client"), { recursive: true });
    await writeFile(resolve(root, "db/_generated/client/functions.json"), '{}');
    await expect(startService(config)).rejects.toThrow("run hulla-db generate with Bun before serve");
  } finally { await rm(root, { recursive: true, force: true }); }
});
