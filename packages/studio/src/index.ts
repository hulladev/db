import { createHmac, randomBytes, timingSafeEqual } from "node:crypto";
import { readFile } from "node:fs/promises";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { DatabaseError } from "@hulla/db";
import { defineConfig, type DatabaseConnection, type InvocationLimits } from "@hulla/db/config";
import { createDatabase, type Database } from "@hulla/db/server";
import { compileSchema, type CompiledTable, type SchemaDefinition } from "@hulla/db/schema";
import type { StudioDeleteResult, StudioPage, StudioRow, StudioSchema } from "./types.ts";
export type { StudioField, StudioTable, StudioSchema, StudioRow, StudioPage, StudioDeleteResult } from "./types.ts";

export interface StudioOptions {
  readonly schema: SchemaDefinition;
  readonly connection: DatabaseConnection;
  readonly limits?: InvocationLimits | undefined;
  readonly port?: number;
  readonly readOnly?: boolean;
  /** A supervisor may retain access across development worker restarts. */
  readonly accessToken?: string | undefined;
}
export interface Studio { readonly url: string; readonly closed: Promise<void>; close(): Promise<void> }

class HTTPError extends Error {
  readonly status: number;
  constructor(status: number, message: string) { super(message); this.status = status; }
}
function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new HTTPError(400, "Expected an object");
  return value as Record<string, unknown>;
}
function keys(value: Record<string, unknown>, allowed: readonly string[]) {
  if (Object.keys(value).some(key => !allowed.includes(key))) throw new HTTPError(400, "Unknown request field");
}
function wire(value: unknown): unknown {
  if (typeof value === "bigint") return value.toString();
  if (Array.isArray(value)) return value.map(wire);
  if (value && typeof value === "object") return Object.fromEntries(Object.keys(value).sort().map(key => [key, wire((value as Record<string, unknown>)[key])]));
  return value;
}
function stored(table: CompiledTable, row: Record<string, unknown>): Record<string, unknown> {
  return Object.fromEntries(Object.entries(table.fields).map(([name, field]) => [name, wire(field.encode(row[name]))]));
}
function equalSecret(actual: string, expected: string): boolean {
  const left = Buffer.from(actual), right = Buffer.from(expected);
  return left.length === right.length && timingSafeEqual(left, right);
}
async function body(request: IncomingMessage): Promise<Record<string, unknown>> {
  if (request.headers["content-type"]?.split(";")[0]?.trim() !== "application/json") throw new HTTPError(415, "Use application/json");
  const chunks: Buffer[] = [];
  let bytes = 0;
  for await (const chunk of request) {
    bytes += chunk.length;
    if (bytes > 1024 * 1024) throw new HTTPError(413, "Request exceeds 1 MiB");
    chunks.push(Buffer.from(chunk));
  }
  try { return record(JSON.parse(Buffer.concat(chunks).toString("utf8"))); }
  catch { throw new HTTPError(400, "Invalid JSON object"); }
}
function json(response: ServerResponse, status: number, value: unknown) {
  const text = JSON.stringify(value);
  if (Buffer.byteLength(text) > 8 * 1024 * 1024) throw new HTTPError(413, "Result exceeds 8 MiB; use a smaller page");
  response.writeHead(status, { "Content-Type": "application/json; charset=utf-8" });
  response.end(text);
}

/** Private loopback operator surface. Never mounts on application function routes. */
export async function startStudio(options: StudioOptions): Promise<Studio> {
  const connection = defineConfig({ database: { server: options.connection } }).database.server;
  const port = options.port ?? 4983;
  if (!Number.isInteger(port) || port < 0 || port > 65535) throw new TypeError("Invalid Studio port");
  const readOnly = options.readOnly ?? process.env.NODE_ENV === "production";
  if (typeof readOnly !== "boolean") throw new TypeError("Invalid Studio access mode");
  const tables = new Map(compileSchema(options.schema).map(table => [table.name, table]));
  const assets = new Map<string, { content: Buffer; type: string }>();
  const directory = new URL(import.meta.url.endsWith(".ts") ? "../dist/" : "./", import.meta.url);
  try {
    for (const [path, file, type] of [
      ["/", "index.html", "text/html; charset=utf-8"],
      ["/theme.js", "theme.js", "text/javascript; charset=utf-8"], ["/style.css", "style.css", "text/css; charset=utf-8"],
      ["/favicon.svg", "favicon.svg", "image/svg+xml"],
      ["/fonts/schibsted-grotesk.woff2", "fonts/schibsted-grotesk.woff2", "font/woff2"],
      ["/fonts/ibm-plex-mono.woff2", "fonts/ibm-plex-mono.woff2", "font/woff2"],
    ]) {
      assets.set(path!, { content: await readFile(new URL(file!, directory)), type: type! });
    }
    const scripts: unknown = JSON.parse(await readFile(new URL("browser-assets.json", directory), "utf8"));
    if (!Array.isArray(scripts) || !scripts.includes("app.js") || scripts.length > 32
      || scripts.some(file => typeof file !== "string" || !(file === "app.js" || /^chunks\/[A-Za-z0-9_-]+\.js$/.test(file)))) throw new Error("Invalid browser asset manifest");
    for (const file of scripts as string[]) {
      assets.set(`/${file}`, { content: await readFile(new URL(file, directory)), type: "text/javascript; charset=utf-8" });
    }
  } catch { throw new Error("Studio browser assets are missing; build @hulla/db-studio before starting it"); }
  const token = options.accessToken ?? randomBytes(32).toString("base64url"), signingKey = randomBytes(32);
  if (!/^[A-Za-z0-9_-]{43}$/.test(token)) throw new TypeError("Studio access tokens must be 32-byte base64url secrets");
  const snapshot = (table: CompiledTable, values: Record<string, unknown>) => createHmac("sha256", signingKey).update(JSON.stringify(wire([table.name, values]))).digest("hex");
  const row = (table: CompiledTable, value: Record<string, unknown>): StudioRow => {
    const values = stored(table, value);
    return { values, snapshot: snapshot(table, values) };
  };
  const metadata: StudioSchema = {
    namespace: connection.namespace ?? "hulla", readOnly,
    tables: [...tables.values()].map(table => ({
      name: table.name, primaryKey: table.primaryKey, keyGeneration: table.keyGeneration,
      indexes: table.indexes, unique: table.unique, checks: wire(table.checks),
      fields: Object.entries(table.fields).map(([name, field]) => ({
        name, scalar: field.scalar, nullable: field.allowsNull, hasDefault: field.hasDefault,
        ...(field.hasDefault ? { default: wire(field.encode(field.defaultValue)) } : {}),
        ...(field.target === undefined ? {} : { reference: String(field.target), onDelete: field.onDelete ?? "restrict" }),
        ...(field.constraints ? { constraints: wire(field.constraints) } : {}),
        ...(field.precision === undefined ? {} : { precision: field.precision }),
        ...(field.jsonShape ? { jsonShape: field.jsonShape } : {}),
      })),
    })),
  };
  const database: Database<SchemaDefinition> = createDatabase(options.schema, connection, {
    ...options.limits,
    readRows: Math.min(options.limits?.readRows ?? 1000, 1000),
    readBytes: Math.min(options.limits?.readBytes ?? 8 * 1024 * 1024, 8 * 1024 * 1024),
  });
  function tableFor(value: unknown): CompiledTable {
    if (typeof value !== "string" || !tables.has(value)) throw new HTTPError(400, "Unknown table");
    return tables.get(value)!;
  }
  async function page(input: Record<string, unknown>): Promise<StudioPage> {
    keys(input, ["table", "filters", "sort", "limit", "cursor"]);
    const table = tableFor(input.table);
    const limit = input.limit ?? 50;
    if (typeof limit !== "number" || !Number.isInteger(limit) || limit < 1 || limit > 200) throw new HTTPError(400, "Page size must be from 1 to 200");
    if (input.cursor !== undefined && input.cursor !== null && (typeof input.cursor !== "string" || input.cursor.length > 65536)) throw new HTTPError(400, "Invalid page cursor");
    let query = database.select(table.name);
    if (input.filters !== undefined) {
      if (!Array.isArray(input.filters) || input.filters.length > 12) throw new HTTPError(400, "Use at most 12 filters");
      for (const item of input.filters) {
        const filter = record(item);
        keys(filter, ["field", "operator", "value"]);
        const field = typeof filter.field === "string" ? table.fields[filter.field] : undefined;
        const operator = filter.operator;
        if (!field || typeof operator !== "string" || !["=", "!=", ">", ">=", "<", "<=", "contains", "is", "is not"].includes(operator)) throw new HTTPError(400, "Invalid filter");
        const value = operator === "contains" ? filter.value : filter.value === null ? null : field.decode(filter.value);
        if (operator === "contains" && field.scalar !== "string") throw new HTTPError(400, "Contains requires a text field");
        // The public query builder owns operator/type validation and SQL compilation.
        query = query.where(filter.field as string, operator as "=", value);
      }
    }
    if (input.sort !== undefined) {
      const sort = record(input.sort);
      keys(sort, ["field", "direction"]);
      if (typeof sort.field !== "string" || !table.fields[sort.field] || !["asc", "desc"].includes(String(sort.direction))) throw new HTTPError(400, "Invalid sort");
      query = query.orderBy(sort.field, sort.direction as "asc" | "desc");
    }
    const result = await query.paginate({ limit, cursor: input.cursor as string | null | undefined ?? null });
    return { rows: result.page.map(value => row(table, value)), cursor: result.cursor };
  }
  async function update(input: Record<string, unknown>): Promise<StudioRow> {
    if (readOnly) throw new HTTPError(403, "Studio is read-only");
    keys(input, ["table", "key", "snapshot", "patch"]);
    const table = tableFor(input.table), patch = record(input.patch);
    if (!Object.keys(patch).length || Object.keys(patch).some(name => name === table.primaryKey || !Object.hasOwn(table.fields, name))) throw new HTTPError(400, "Patch must contain editable fields; primary keys cannot change");
    if (typeof input.snapshot !== "string" || !/^[a-f0-9]{64}$/.test(input.snapshot)) throw new HTTPError(400, "Invalid row snapshot");
    const expected = input.snapshot;
    const key = table.fields[table.primaryKey]!.decode(input.key);
    const decoded = Object.fromEntries(Object.entries(patch).map(([name, value]) => [name, table.fields[name]!.decode(value)]));
    return database.transaction(async tx => {
      const current = await tx.select(table.name).where({ [table.primaryKey]: key }).unique();
      if (!current || !equalSecret(snapshot(table, stored(table, current)), expected)) throw new HTTPError(409, "This row changed or was deleted. Reload it before saving.");
      const result = await tx.update(table.name, decoded).where({ [table.primaryKey]: key }).expectRows(1);
      return row(table, result[0]!);
    }, { retries: 2 });
  }
  async function deleteRows(input: Record<string, unknown>): Promise<StudioDeleteResult> {
    if (readOnly) throw new HTTPError(403, "Studio is read-only");
    keys(input, ["table", "rows"]);
    const table = tableFor(input.table);
    if (!Array.isArray(input.rows) || input.rows.length < 1 || input.rows.length > 200) throw new HTTPError(400, "Select from 1 to 200 rows");
    const primary = table.fields[table.primaryKey]!;
    const expected = new Map<string, string>();
    const identities = input.rows.map(item => {
      const selection = record(item); keys(selection, ["key", "snapshot"]);
      if (typeof selection.snapshot !== "string" || !/^[a-f0-9]{64}$/.test(selection.snapshot)) throw new HTTPError(400, "Invalid row snapshot");
      const key = primary.decode(selection.key), identity = JSON.stringify(wire(primary.encode(key)));
      if (expected.has(identity)) throw new HTTPError(400, "Select each row only once");
      expected.set(identity, selection.snapshot);
      return key;
    });
    return database.transaction(async tx => {
      const current = await tx.select(table.name).where(table.primaryKey, "in", identities).limit(identities.length);
      if (current.length !== identities.length || current.some(value => {
        const values = stored(table, value), identity = JSON.stringify(values[table.primaryKey]);
        return !equalSecret(snapshot(table, values), expected.get(identity) ?? "");
      })) throw new HTTPError(409, "A selected row changed or was deleted. Refresh the table and select the rows again.");
      await tx.delete(table.name).where(table.primaryKey, "in", identities).expectRows(identities.length);
      return { deleted: identities.length };
    }, { retries: 2 });
  }
  let origin = "", active = 0;
  const server = createServer((request, response) => {
    response.setHeader("Cache-Control", "no-store");
    response.setHeader("X-Content-Type-Options", "nosniff");
    response.setHeader("Referrer-Policy", "no-referrer");
    response.setHeader("Content-Security-Policy", "default-src 'none'; script-src 'self'; style-src 'self'; font-src 'self'; img-src 'self'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'");
    void (async () => {
      if (`http://${request.headers.host}` !== origin || (request.headers.origin !== undefined && request.headers.origin !== origin)) throw new HTTPError(403, "Studio only accepts requests from its own loopback origin");
      if (active >= 16) throw new HTTPError(503, "Studio is busy; try again shortly");
      active++;
      try {
        const path = new URL(request.url ?? "/", origin).pathname;
        const asset = assets.get(path);
        if (asset && request.method === "GET") {
          if (asset.type === "font/woff2") response.setHeader("Cache-Control", "private, max-age=86400");
          response.writeHead(200, { "Content-Type": asset.type }); response.end(asset.content); return;
        }
        if (!path.startsWith("/api/")) throw new HTTPError(404, "Not found");
        if (!equalSecret(request.headers.authorization ?? "", `Bearer ${token}`)) throw new HTTPError(401, "Open Studio using the access link printed in your terminal");
        if (path === "/api/schema" && request.method === "GET") json(response, 200, metadata);
        else if (path === "/api/rows" && request.method === "POST") json(response, 200, await page(await body(request)));
        else if (path === "/api/update" && request.method === "POST") json(response, 200, await update(await body(request)));
        else if (path === "/api/delete" && request.method === "POST") json(response, 200, await deleteRows(await body(request)));
        else throw new HTTPError(404, "Not found");
      } finally { active--; }
    })().catch(error => {
      if (response.destroyed || response.writableEnded) return;
      if (error instanceof HTTPError) json(response, error.status, { error: error.message });
      else if (error instanceof TypeError) json(response, 400, { error: "Invalid field value or query. Check the schema and use database scalar values." });
      else if (error instanceof DatabaseError) {
        const conflict = ["serialization_conflict", "row_count_mismatch"].includes(error.code);
        json(response, conflict ? 409 : 422, { error: conflict ? "This row changed. Reload it before saving." : error.message, code: error.code });
      } else json(response, 500, { error: "Studio could not complete this operation" });
    });
  });
  server.requestTimeout = 10_000;
  server.headersTimeout = 5_000;
  server.keepAliveTimeout = 2_000;
  server.maxConnections = 32;
  let finish!: () => void;
  const closed = new Promise<void>(resolve => { finish = resolve; });
  let closing: Promise<void> | undefined;
  const close = () => closing ??= (async () => {
    const stopped = new Promise<void>(resolve => server.close(() => resolve()));
    server.closeAllConnections();
    await Promise.all([database.close(), stopped]);
    finish();
  })();
  try {
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(port, "127.0.0.1", () => { server.off("error", reject); resolve(); });
    });
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("Studio listener unavailable");
    origin = `http://127.0.0.1:${address.port}`;
    server.on("error", () => { void close(); });
    server.on("close", () => { if (!closing) void close(); });
    return { url: `${origin}/#token=${token}`, closed, close };
  } catch (error) {
    await close();
    if ((error as NodeJS.ErrnoException).code === "EADDRINUSE") throw Object.assign(new Error(`Studio port ${port} is in use; choose another with --studio-port`), { code: "EADDRINUSE" });
    throw error;
  }
}
