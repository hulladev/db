import assert from "node:assert/strict";
import { DatabaseError, type MutationState, type QueryState } from "../src/index.ts";
import { HttpRuntime } from "../src/http.ts";
import { runtime, contracts } from "./generated/runtime.ts";
import { create, fail, list } from "./generated/deployed.ts";

const settings = JSON.parse(process.env.HULLADB_SDK_FIXTURE!) as { baseUrl: string; alice: { scope: string; principal: string; authorizationScope: string; bearer: string }; bob: { scope: string; principal: string; authorizationScope: string; bearer: string } };
let lose = true, offlineRecovery = true, streams = 0, mutationPosts = 0, receiptChecks = 0, queryPosts = 0, disconnect: AbortController | undefined;
const sessionTimes: number[] = [], mutationTimes: number[] = [];
const transport = (async (url: string | URL | Request, options?: RequestInit) => {
  const path = new URL(String(url)).pathname;
  if (path.startsWith("/functions/receipts/") && !path.endsWith("/ack")) {
    receiptChecks++;
    if (offlineRecovery) throw new Error("injected recovery outage");
  }
  if (path === "/functions/mutate") mutationPosts++;
  if (path === "/functions/query") queryPosts++;
  if (path === "/functions/subscriptions") {
    streams++;
    disconnect = new AbortController();
    options = { ...options, signal: AbortSignal.any([options!.signal!, disconnect.signal]) };
  }
  const started = performance.now();
  const response = await fetch(url, options);
  if (path === "/functions/session" || path === "/functions/mutate") {
    await response.clone().arrayBuffer();
    (path === "/functions/session" ? sessionTimes : mutationTimes).push(performance.now() - started);
  }
  if (path === "/functions/mutate" && lose && response.ok) {
    lose = false; await response.arrayBuffer(); throw new Error("injected acknowledgement loss after PostgreSQL commit");
  }
  return response;
}) as typeof fetch;
const adapter = new HttpRuntime({ baseUrl: settings.baseUrl, auth: { key: "alice" }, headers: { authorization: `Bearer ${settings.alice.bearer}` }, contracts, fetch: transport, reconnectDelayMs: 10, operationTimeoutMs: 2000, confirmationTimeoutMs: 1500 });
const optimisticCreate = create.withOptimistic({
  query: list,
  input: () => undefined,
  update: (rows, input) => [...rows, { id: "optimistic-fixture-row" as Awaited<ReturnType<typeof create>>["id"], title: input.title, ordinal: input.ordinal, owner: "alice" }],
});
const unconfigure = runtime.configure(() => adapter);
const states: QueryState<Awaited<ReturnType<typeof list>>>[] = [];
const stop = list.subscribe(state => states.push(state));
async function until(condition: () => boolean) {
  const deadline = Date.now() + 5000;
  while (!condition()) { if (Date.now() > deadline) throw new Error("subscription condition timed out"); await Bun.sleep(5); }
}
try {
  await until(() => states.at(-1)?.status === "success");
  const checksBefore = receiptChecks, postsBefore = mutationPosts;
  const firstCall = optimisticCreate({ title: "PostgreSQL native", ordinal: -9223372036854775808n });
  const mutationStates: MutationState<Awaited<typeof firstCall>>[] = [];
  const stopMutation = firstCall.subscribe(state => mutationStates.push(state));
  await until(() => receiptChecks >= checksBefore + 2);
  assert.ok(mutationStates.some(state => state.status === "recovering"));
  assert.ok(states.some(state => state.status === "success" && state.isOptimistic));
  assert.ok(states.at(-1)?.status === "success" && !states.at(-1)!.isOptimistic);
  offlineRecovery = false;
  const first = await firstCall;
  stopMutation();
  assert.equal(mutationPosts, postsBefore + 1, "automatic receipt recovery resubmitted a committed mutation");
  assert.ok(states.filter(state => state.status === "success").every(state => state.data.length <= 1));
  assert.equal(first.ordinal, -9223372036854775808n);
  assert.equal(first.owner, "alice");
  assert.deepEqual(await list(), [first]);
  await until(() => states.at(-1)?.status === "success" && (states.at(-1) as { data: readonly unknown[] }).data.length === 1);
  disconnect!.abort();
  await until(() => streams >= 2 && states.at(-1)?.status === "success" && !states.at(-1)!.isStale);
  await assert.rejects(Promise.resolve(fail({ title: "must roll back", ordinal: 9223372036854775807n })));
  assert.deepEqual(await list(), [first]);
  lose = true; offlineRecovery = true;
  await assert.rejects(Promise.resolve(create({ title: "committed before confirmation expiry", ordinal: 9223372036854775807n })), (error: unknown) => error instanceof DatabaseError && error.code === "confirmation_timeout");
  stop(); await adapter.dispose(); offlineRecovery = false;
  let bearer = settings.alice.bearer;
  const restarted = new HttpRuntime({ baseUrl: settings.baseUrl, auth: { key: "alice" }, headers: () => ({ authorization: `Bearer ${bearer}` }), contracts, fetch: transport, reconnectDelayMs: 10 });
  try {
    const postsAtRestart = mutationPosts, queriesAtRestart = queryPosts;
    const rows = await restarted.call({ name: "deployed.list", kind: "query", input: undefined }) as typeof first[];
    assert.equal(rows.length, 2);
    assert.ok(rows.some(row => row.title === "committed before confirmation expiry" && row.ordinal === 9223372036854775807n));
    assert.equal(queryPosts, queriesAtRestart + 1, "fresh runtime did not read from the server");
    assert.equal(mutationPosts, postsAtRestart, "fresh runtime resubmitted a previous mutation");
    bearer = settings.bob.bearer;
    restarted.setAuth({ key: "bob" });
    assert.deepEqual(await restarted.call({ name: "deployed.list", kind: "query", input: undefined }), []);
    const watched: QueryState<unknown>[] = [];
    const stopBob = restarted.watch({ name: "deployed.list", kind: "query", input: undefined }, state => watched.push(state));
    await until(() => watched.at(-1)?.status === "success");
    await fetch(settings.baseUrl + "/fixture/revoke", { method: "POST" });
    await until(() => watched.at(-1)?.status === "error");
    stopBob();
  } finally { await restarted.dispose(); }
  const head = await (await transport(settings.baseUrl + "/functions/session", { headers: { authorization: "Bearer alice" } })).json() as { source: string; ticket: { id: string } };
  const rejected = await fetch(settings.baseUrl + "/functions/mutate", {
    method: "POST", headers: { authorization: "Bearer alice", "content-type": "application/json" },
    body: JSON.stringify({ request_id: crypto.randomUUID(), ticket: head.ticket.id, name: "deployed.create", input: { title: "wrong source", ordinal: "1" }, expected_source: crypto.randomUUID() }),
  });
  assert.equal(rejected.status, 409);
  assert.equal((await rejected.json() as { code: string }).code, "reset_required");
  const oldReceipt = await fetch(settings.baseUrl + `/functions/receipts/${crypto.randomUUID()}?source=${crypto.randomUUID()}&ticket=${head.ticket.id}`, { headers: { authorization: "Bearer alice" } });
  assert.equal(oldReceipt.status, 409);
  assert.ok(head.source);
  const cors = await fetch(settings.baseUrl + "/functions/query", { method: "OPTIONS", headers: { origin: "https://sdk.example.test", "access-control-request-method": "POST" } });
  assert.equal(cors.status, 204);
  assert.equal(cors.headers.get("access-control-allow-origin"), "https://sdk.example.test");
  console.log(JSON.stringify({ localTicketTimingMs: { coldSetup: sessionTimes[0], renewalEndpoint: sessionTimes.at(-1), mutationWithAdmission: mutationTimes[0] } }));
  console.log("Generic SDK/PostgreSQL integration passed: typed int64, layered optimism, serializable mutations, atomic rollback, automatic receipt confirmation, deadline errors, authoritative restart reads, SSE reconnect, identity cleanup, revocation and CORS.");
} finally { stop(); unconfigure(); await adapter.dispose(); }
