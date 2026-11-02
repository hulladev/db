import assert from "node:assert/strict";
import { request } from "node:http";
import { readFile } from "node:fs/promises";
import { defineSchema, defineTable, v } from "@hulla/db/schema";
import { startStudio } from "../src/index.ts";

const studio = await startStudio({
  schema: defineSchema({ documents: defineTable({ count: v.int64({ mode: "bigint" }).default(9223372036854775807n), payload: v.json() }) }),
  connection: { url: "postgresql://unused@127.0.0.1:1/unused" }, readOnly: true, port: 0,
});
const url = new URL(studio.url), token = new URLSearchParams(url.hash.slice(1)).get("token")!;
const headers = { Authorization: `Bearer ${token}`, "Content-Type": "application/json" };
try {
  await assert.rejects(startStudio({ schema: defineSchema({}), connection: { url: "postgresql://unused/unused" }, port: Number(url.port) }), { code: "EADDRINUSE" });
  const html = await fetch(url.origin);
  assert.equal(html.status, 200);
  assert.match(html.headers.get("content-security-policy")!, /frame-ancestors 'none'/);
  assert.equal(html.headers.get("cache-control"), "no-store");
  const scripts = JSON.parse(await readFile(new URL("../dist/browser-assets.json", import.meta.url), "utf8")) as string[];
  for (const asset of [...scripts, "theme.js", "style.css", "favicon.svg", "fonts/schibsted-grotesk.woff2", "fonts/ibm-plex-mono.woff2"]) {
    const response = await fetch(`${url.origin}/${asset}`);
    assert.equal(response.status, 200);
    if (asset.endsWith(".woff2")) assert.equal(response.headers.get("content-type"), "font/woff2");
    if (asset.endsWith(".svg")) assert.equal(response.headers.get("content-type"), "image/svg+xml");
    await response.arrayBuffer();
  }
  assert.equal((await fetch(`${url.origin}/api/schema`)).status, 401);
  assert.equal((await fetch(`${url.origin}/api/schema`, { headers: { Authorization: "Bearer invalid" } })).status, 401);
  assert.equal((await fetch(`${url.origin}/api/schema`, { headers: { ...headers, Origin: "https://untrusted.example" } })).status, 403);
  const rebinding = await new Promise<number>(resolve => {
    const req = request(`${url.origin}/api/schema`, { headers: { ...headers, Host: "untrusted.example" } }, response => { response.resume(); resolve(response.statusCode!); });
    req.end();
  });
  assert.equal(rebinding, 403);
  const metadata = await (await fetch(`${url.origin}/api/schema`, { headers })).json();
  assert.equal(metadata.readOnly, true);
  assert.equal(metadata.tables[0].fields.find((field: { name: string }) => field.name === "count").default, "9223372036854775807");
  assert.equal(JSON.stringify(metadata).includes("postgresql:"), false);
  async function post(path: string, input: unknown) {
    return fetch(`${url.origin}/api/${path}`, { method: "POST", headers, body: JSON.stringify(input) });
  }
  assert.equal((await post("update", { table: "documents", patch: { payload: {} } })).status, 403);
  assert.equal((await post("delete", { table: "documents", rows: [] })).status, 403);
  assert.equal((await post("rows", { table: "unknown" })).status, 400);
  assert.equal((await post("rows", { table: "documents", limit: 201 })).status, 400);
  assert.equal((await post("rows", { table: "documents", filters: [{ field: "constructor", operator: "=", value: "x" }] })).status, 400);
  assert.equal((await fetch(`${url.origin}/api/rows`, { method: "POST", headers, body: "{" })).status, 400);
  assert.equal((await fetch(`${url.origin}/api/rows`, { method: "POST", headers: { Authorization: `Bearer ${token}` }, body: "{}" })).status, 415);
  const oversized = await post("rows", { table: "documents", padding: "a".repeat(1024 * 1024) });
  assert.equal(oversized.status, 413);
  // Repeated opening/closing must release the listener; no database process is
  // needed for metadata and rejected requests.
} finally { await studio.close(); await studio.closed; }
await assert.rejects(fetch(url.origin));
const replacement = await startStudio({ schema: defineSchema({}), connection: { url: "postgresql://unused/unused" }, port: Number(url.port) });
await replacement.close();
console.log("Studio HTTP access and lifecycle checks passed");
