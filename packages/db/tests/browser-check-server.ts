const build = await Bun.build({ entrypoints: [new URL("./browser-check.ts", import.meta.url).pathname], target: "browser" });
if (!build.success || !build.outputs[0]) throw new Error("Browser check build failed");
const script = await build.outputs[0].text();
const source = "00000000-0000-0000-0000-000000000004", scope = "00000000-0000-0000-0000-000000000001", authorizationScope = "00000000-0000-0000-0000-000000000003";
const rows: { id: string; title: string; owner: string; ordinal: string }[] = [];
const receipts = new Map<string, unknown>();
const server = Bun.serve({ hostname: "127.0.0.1", port: Number(process.env.HULLADB_BROWSER_CHECK_PORT ?? 0), async fetch(request) {
  const path = new URL(request.url).pathname;
  if (path === "/check.js") return new Response(script, { headers: { "content-type": "text/javascript" } });
  const response = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "content-type": "application/json" } });
  if (path === "/fixture/reset") { rows.length = 0; receipts.clear(); return response(null); }
  if (!path.startsWith("/functions/")) return new Response('<!doctype html><title>SDK browser verification</title><h1>SDK browser verification</h1><p id="status">Running checks…</p><script type="module" src="/check.js"></script>', { headers: { "content-type": "text/html" } });
  const bearer = request.headers.get("authorization");
  const owner = bearer === "Bearer fixture-only" ? "alice" : bearer === "Bearer other-fixture" ? "bob" : undefined;
  if (!owner) return response({ code: "unauthorized" }, 401);
  const principal = owner === "alice" ? "00000000-0000-0000-0000-000000000002" : "00000000-0000-0000-0000-000000000005";
  const head = () => ({ source, scope, principal, authorization_scope: authorizationScope, revision: String(rows.length) });
  const visibleRows = () => rows.filter(row => row.owner === owner);
  if (path === "/functions/session") return response({ ...head(), ticket: { id: crypto.randomUUID(), expires_in_ms: 60_000, renew_after_ms: 30_000 } });
  if (path === "/functions/query") return response({ ...head(), result: visibleRows() });
  if (path.endsWith("/ack")) return response(null);
  if (path.startsWith("/functions/receipts/")) return response(receipts.get(`${principal}:${path.split("/").at(-1)!}`) ?? null);
  if (path === "/functions/mutate") {
    const body = await request.json() as { request_id: string; input: { title: string; ordinal: string } };
    const row = { id: crypto.randomUUID(), title: body.input.title, owner, ordinal: body.input.ordinal };
    rows.push(row);
    const receipt = { request_id: body.request_id, source, scope, revision: String(rows.length), result: row };
    receipts.set(`${principal}:${body.request_id}`, receipt);
    return response(receipt);
  }
  if (path === "/functions/subscriptions") {
    const { queries } = await request.json() as { queries: { key: string }[] };
    const subscription = crypto.randomUUID();
    const frame = { subscription, sequence: "1", round: { ...head(), results: queries.map(({ key }) => ({ key, result: visibleRows() })) } };
    const body = new ReadableStream<Uint8Array>({ start(controller) { controller.enqueue(new TextEncoder().encode(`id: ${subscription}:1\nevent: round\ndata: ${JSON.stringify(frame)}\n\n`)); } });
    return new Response(body, { headers: { "content-type": "text/event-stream" } });
  }
  return response({ code: "not_found" }, 404);
} });
console.log(server.url.href);
