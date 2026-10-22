import { DatabaseError, type MutationState, type QueryState } from "../src/index.ts";
import { HttpRuntime } from "../src/http.ts";
import { contracts, runtime } from "./generated/runtime.ts";
import { create, list } from "./generated/deployed.ts";

const status = document.querySelector("#status")!;
const session = { scope: "00000000-0000-0000-0000-000000000001", principal: "00000000-0000-0000-0000-000000000002", authorizationScope: "00000000-0000-0000-0000-000000000003", bearer: "fixture-only" };
const baseUrl = location.origin;
let drop = false, unavailable = false, mutationPosts = 0, receiptChecks = 0, queryPosts = 0, unauthorizedResponses = 0;
const peer = (async (url: string | URL | Request, init?: RequestInit) => {
  const path = new URL(String(url)).pathname;
  if (path === "/functions/query") queryPosts++;
  if (path.startsWith("/functions/receipts/") && !path.endsWith("/ack")) {
    receiptChecks++;
    if (unavailable) throw new Error("injected receipt outage");
  }
  if (path === "/functions/mutate") mutationPosts++;
  const response = await fetch(url, init);
  if (response.status === 401) unauthorizedResponses++;
  if (path === "/functions/mutate" && drop && response.ok) {
    drop = false;
    await response.arrayBuffer();
    throw new Error("injected response loss after commit");
  }
  return response;
}) as typeof fetch;
function check(condition: unknown, reason: string): asserts condition { if (!condition) throw new Error(reason); }
async function until(condition: () => boolean) { const deadline = Date.now() + 3000; while (!condition()) { if (Date.now() > deadline) throw new Error("browser check timed out"); await new Promise(r => setTimeout(r, 5)); } }

async function run() {
  const reload = new URL(location.href).searchParams.has("reload");
  if (!reload) await fetch("/fixture/reset", { method: "POST" });
  const sdk = new HttpRuntime({ baseUrl, auth: { key: "fixture" }, headers: { authorization: `Bearer ${session.bearer}` }, contracts, fetch: peer, reconnectDelayMs: 5, confirmationTimeoutMs: 2000 });
  const unconfigure = runtime.configure(() => sdk);
  try {
    if (reload) {
      const rows = await list();
      check(queryPosts === 1, "reload did not fetch authoritative query data");
      check(rows.length === 3 && rows[0]?.ordinal === -9223372036854775808n && rows[1]?.ordinal === 9223372036854775807n, "reload lost committed rows or int64 precision");
      status.textContent = "PASS: real page reload fetches authoritative results with lossless int64";
      return;
    }
    const created = await create({ title: "browser", ordinal: -9223372036854775808n });
    check(created.ordinal === -9223372036854775808n, "Bigint result lost precision");
    check((await list()).length === 1, "fresh query did not retrieve committed data");
    drop = true; unavailable = true;
    const checksBefore = receiptChecks, postsBefore = mutationPosts;
    const mutation = create({ title: "automatic confirmation", ordinal: 9223372036854775807n });
    const mutationStates: MutationState<Awaited<typeof mutation>>[] = [];
    const stopMutation = mutation.subscribe(state => mutationStates.push(state));
    await until(() => receiptChecks >= checksBefore + 2);
    check(mutationStates.some(state => state.status === "recovering"), "mutation did not report interrupted confirmation");
    unavailable = false;
    check((await mutation).ordinal === 9223372036854775807n, "same-handle receipt recovery lost precision");
    check(mutationPosts === postsBefore + 1, "receipt recovery resubmitted the mutation");
    stopMutation();

    drop = true; unavailable = true;
    const expired = create({ title: "committed before confirmation expiry", ordinal: 7n });
    try { await expired; throw new Error("confirmation unexpectedly succeeded"); }
    catch (error) { check(error instanceof DatabaseError && error.code === "confirmation_timeout", "confirmation expiry was not distinguishable from execution failure"); }
    unavailable = false;
  } finally { unconfigure(); await sdk.dispose(); }

  let bearer: string | undefined = session.bearer;
  const restarted = new HttpRuntime({ baseUrl, auth: { key: "fixture" }, headers: () => bearer ? { authorization: `Bearer ${bearer}` } : {}, contracts, fetch: peer, reconnectDelayMs: 5 });
  const configureRestarted = runtime.configure(() => restarted);
  try {
    const before = queryPosts;
    check((await list()).length === 3 && queryPosts === before + 1, "new runtime did not fetch committed rows authoritatively");
    const states: QueryState<Awaited<ReturnType<typeof list>>>[] = [];
    const stop = list.subscribe(state => states.push(state));
    await until(() => states.at(-1)?.status === "success");
    bearer = "other-fixture";
    restarted.setAuth({ key: "other-fixture" });
    const retired = states.at(-1);
    check(retired?.status === "error" && retired.data === undefined, "identity switch retained private query data");
    check((await list()).length === 0, "identity switch returned another account's rows");
    stop();

    const completed = create({ title: "bob private result", ordinal: 8n });
    const completedStates: MutationState<Awaited<typeof completed>>[] = [];
    const stopCompleted = completed.subscribe(state => completedStates.push(state));
    await completed;
    const current = list();
    const currentStates: QueryState<Awaited<typeof current>>[] = [];
    await current;
    const stopCurrent = current.subscribe(state => currentStates.push(state), { keepPreviousDataOnError: true });
    await until(() => currentStates.at(-1)?.status === "success");
    check(currentStates.at(-1)?.data?.[0]?.owner === "bob", "signed-in query did not show the current user's data");
    // The app's auth subscription owns expiration and retires the SDK scope explicitly.
    bearer = undefined;
    restarted.setAuth(null);
    const retiredMutation = completedStates.at(-1);
    check(retiredMutation?.status === "error" && !("data" in retiredMutation), "sign-out retained a completed mutation result in its observer");
    check(currentStates.at(-1)?.status === "error" && currentStates.at(-1)?.data === undefined, "sign-out retained query data despite auth retirement");
    const deniedBefore = unauthorizedResponses;
    try { await list(); throw new Error("expired credentials unexpectedly authorized a fresh query"); }
    catch (error) { check(error instanceof DatabaseError && error.code === "context_changed" && unauthorizedResponses === deniedBefore + 1, "the server did not reject missing credentials independently of client auth state"); }
    stopCompleted(); stopCurrent();
  } finally { configureRestarted(); await restarted.dispose(); }

  const offline = new HttpRuntime({ baseUrl, auth: { key: "fixture" }, headers: { authorization: `Bearer ${session.bearer}` }, contracts, fetch: (async (_request: string | URL | Request): Promise<Response> => { throw new Error("offline"); }) as typeof fetch, reconnectDelayMs: 5 });
  const configureOffline = runtime.configure(() => offline);
  try {
    const states: QueryState<unknown>[] = [];
    const stop = list.subscribe(state => states.push(state));
    await until(() => states.some(state => state.status === "error"));
    check(states.every(state => state.data === undefined), "fresh offline runtime hydrated private query results");
    stop();
  } finally { configureOffline(); await offline.dispose(); }
  status.textContent = "PASS: automatic same-handle confirmation, deadline errors, authoritative restart reads, lossless int64, identity cleanup, and app-driven sign-out";
}
void run().catch(error => { status.textContent = `FAIL: ${String(error)}`; console.error(error); });
