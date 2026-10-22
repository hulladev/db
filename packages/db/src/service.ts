import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { access, readFile } from "node:fs/promises";
import { constants } from "node:fs";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { defineConfig, type VerifyIdentity } from "./config.ts";
import { resolveNativeExecutable } from "./native.ts";
import { schemaDescriptor } from "./schema-contract.ts";

class VerificationTimeout extends Error {}

function canonical(value: unknown, depth = 0): unknown {
  if (depth > 64) throw new TypeError("Identity nesting exceeds the supported depth");
  if (value === null || typeof value === "string" || typeof value === "boolean") return value;
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (Array.isArray(value)) return value.map(item => canonical(item, depth + 1));
  if (typeof value === "object" && value !== null && [Object.prototype, null].includes(Object.getPrototypeOf(value))) {
    const result: Record<string, unknown> = Object.create(null);
    for (const key of Reflect.ownKeys(value).sort()) {
      if (typeof key !== "string") throw new TypeError("Identity must be JSON data");
      result[key] = canonical((value as Record<string, unknown>)[key], depth + 1);
    }
    return result;
  }
  throw new TypeError("Identity must be JSON data");
}
function identityId(value: unknown): string {
  const hex = createHash("sha256").update(JSON.stringify(value)).digest("hex").slice(0, 32);
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

/** Private bridge context. Only a trusted verifier result can reach this function. */
export async function verifyServiceIdentity(verify: VerifyIdentity | undefined, headers: Headers) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let verified;
  try {
    verified = verify ? await Promise.race([
      Promise.resolve().then(() => verify(headers)),
      new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new VerificationTimeout("Authentication verification timed out")), 5000); }),
    ]) : null;
  } finally { clearTimeout(timer); }
  if (verified !== null && (typeof verified !== "object" || typeof verified.key !== "string" || !verified.key.trim() || verified.key.length > 1024))
    throw new TypeError("Auth verifier must return a key and JSON identity, or null");
  const identity = verified === null ? null : canonical(verified.identity);
  if (Buffer.byteLength(JSON.stringify(identity)) > 1024 * 1024) throw new TypeError("Verified identity exceeds the transport budget");
  const principal = verified === null ? ["anonymous"] : ["authenticated", verified.key];
  return { principal: identityId(principal), authorizationScope: identityId([principal, identity]), identity };
}

/** Runtime-only launcher. The native host owns HTTP/SSE, transactions and revalidation. */
export async function startService(configPath: string): Promise<{ url: string; close(): Promise<void>; closed: Promise<void> }> {
  const config = defineConfig((await import(pathToFileURL(resolve(configPath)).href)).default);
  const schemaPath = resolve(dirname(resolve(configPath)), config.schema ?? "db/schema.ts");
  const schema = (await import(pathToFileURL(schemaPath).href)).default;
  const generated = resolve(dirname(schemaPath), "_generated");
  const bundle = resolve(generated, "worker.js");
  let manifest: string;
  try {
    manifest = await readFile(resolve(generated, "client/functions.json"), "utf8");
    await access(bundle, constants.R_OK);
  } catch {
    throw new Error("Cannot read generated service artifacts; run hulla-db generate with Bun before serve and include the generated directory in your deployment");
  }
  const executable = resolveNativeExecutable("service");
  const child = spawn(executable, [], { stdio: ["pipe", "pipe", "pipe"], env: {} });
  let diagnostics = "";
  child.stderr.on("data", (chunk: Buffer) => {
    diagnostics += chunk.toString("utf8");
    const lines = diagnostics.split("\n"); diagnostics = lines.pop()!.slice(-4096);
    for (const line of lines) if (line.startsWith("[hulla/db] ")) console.warn(line);
  });
  let buffer = Buffer.alloc(0), stopping = false, settled = false, pendingAuth = 0;
  let ready!: (url: string) => void, startupFailed!: (error: Error) => void;
  const startup = new Promise<string>((resolve, reject) => { ready = resolve; startupFailed = reject; });
  let exited!: () => void, failed!: (error: Error) => void;
  const closed = new Promise<void>((resolve, reject) => { exited = resolve; failed = reject; });
  void closed.catch(() => {});
  const fail = (message: string) => {
    const error = new Error(message);
    startupFailed(error); failed(error); child.kill();
  };
  const send = (frame: unknown) => { if (!stopping && !child.stdin.destroyed) child.stdin.write(JSON.stringify(frame) + "\n"); };
  child.on("error", () => fail("Cannot start hulladb-service; build the native service or set HULLADB_SERVICE"));
  child.stdin.on("error", () => { if (!stopping) fail("Database service disconnected"); });
  child.on("exit", code => {
    settled = true;
    startupFailed(new Error("Database service exited before startup completed"));
    if (stopping || code === 0) exited(); else failed(new Error("Database service stopped unexpectedly"));
  });
  child.stdout.on("data", (chunk: Buffer) => {
    buffer = Buffer.concat([buffer, chunk]);
    if (buffer.length > 66 * 1024 * 1024) { fail("Database service frame exceeds its transport budget"); return; }
    let newline: number;
    while ((newline = buffer.indexOf(10)) !== -1) {
      const line = buffer.subarray(0, newline); buffer = buffer.subarray(newline + 1);
      let frame: Record<string, unknown>;
      try { frame = JSON.parse(line.toString("utf8")); if (!frame || typeof frame !== "object") throw new Error(); }
      catch { fail("Invalid database service response"); return; }
      if (frame.type === "ready" && typeof frame.url === "string") ready(frame.url);
      else if (frame.type === "authenticate" && Number.isSafeInteger(frame.id) && Array.isArray(frame.headers)) {
        const { id, headers } = frame;
        if (pendingAuth >= 64) { send({ type: "authenticated", id, context: null, unavailable: true }); continue; }
        pendingAuth++;
        void (async () => {
          let context: Awaited<ReturnType<typeof verifyServiceIdentity>> | null = null;
          let unavailable = false;
          try { context = await verifyServiceIdentity(config.auth, new Headers(headers as [string, string][])); }
          catch (error) { unavailable = error instanceof VerificationTimeout; }
          finally { pendingAuth--; }
          send({ type: "authenticated", id, context, unavailable });
        })();
      } else if (frame.type === "error" && typeof frame.message === "string") {
        // The supplied native binary emits only static, sanitized diagnostics.
        fail(frame.message); return;
      } else { fail("Database service startup or protocol failed"); return; }
    }
  });
  async function close(): Promise<void> {
    if (!stopping) {
      if (!settled) send({ type: "shutdown" });
      stopping = true;
      child.stdin.end();
    }
    const timer = setTimeout(() => child.kill("SIGKILL"), 2000);
    try { await closed; } finally { clearTimeout(timer); }
  }
  const timer = setTimeout(() => fail("Database service startup timed out"), 30_000);
  try {
    send({ type: "init", url: config.database.server.url, namespace: config.database.server.namespace ?? "hulla",
      schema: schemaDescriptor(schema), manifest, development: process.env.NODE_ENV !== "production",
      worker: {
        executable: resolveNativeExecutable("js"),
        entrypoint: bundle,
      },
      host: config.service?.host ?? "127.0.0.1", port: config.service?.port ?? 3001,
      origins: config.service?.origins ?? [],
    });
    return { url: await startup, close, closed };
  } catch (error) { await close().catch(() => {}); throw error; }
  finally { clearTimeout(timer); }
}
