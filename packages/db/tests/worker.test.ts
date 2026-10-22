import { expect, test } from "bun:test";
import { valueCodec, type ValueShape } from "../src/codec.ts";
import cases from "./fixtures/wire-contracts.json";
import schema from "./fixtures/deployed-schema.ts";
import { schemaDescriptor } from "../src/schema-contract.ts";
import { installWorker } from "../src/worker.ts";
import { defineFunctions } from "../src/declarations.ts";
import type { QueryContext, MutationContext } from "../src/database.ts";
import * as validation from "./fixtures/validation.ts";

const { mutation } = defineFunctions<QueryContext<typeof schema>, MutationContext<typeof schema>>();
let afterSerialization: Promise<unknown> | undefined;
const boundaries = {
  serialization: mutation(ctx => ({
    get label() {
      afterSerialization = ctx.db.insert("entries", { owner: "alice", title: "too late" }).catch(error => error);
      return "safe";
    },
  })),
  leaked: mutation(ctx => {
    void ctx.db.insert("entries", { owner: "alice", title: "unawaited" }).catch(() => {});
    return "early";
  }),
  caught: mutation(async ctx => {
    try { await ctx.db.insert("entries", { owner: "alice", title: "failed" }); } catch {}
    return "caught";
  }),
  expectation: mutation(async ctx => {
    try { await ctx.db.insert("entries", { owner: "alice", title: "count mismatch" }).expectRows(2); } catch {}
    return "caught";
  }),
};
let nativeRequest: (operation: string) => Promise<string> = async () => { throw new Error("Unexpected native request"); };
(globalThis as unknown as { __hullaDb: typeof nativeRequest }).__hullaDb = operation => nativeRequest(operation);
installWorker({ schema, modules: { validation, boundaries } });
const execute = (globalThis as unknown as { __hullaExecute: (value: unknown) => Promise<string> }).__hullaExecute;
const inputShape: ValueShape = { kind: "object", fields: { title: { optional: false, shape: { kind: "string" } } } };
const resultShape: ValueShape = { kind: "object", fields: { label: { optional: false, shape: { kind: "string" } } } };
function invocation(name: string, result_shape: ValueShape = { kind: "string" }) {
  return { schema: schemaDescriptor(schema), name, kind: "mutation", identity: "alice", input_shape: { kind: "undefined" }, input: null, result_shape };
}

test("browser and native callbacks use the same lossless and unambiguous wire contracts", () => {
  for (const item of cases) {
    const codec = valueCodec(item.shape as ValueShape);
    for (const value of item.valid) expect(() => codec.decode(value)).not.toThrow();
    for (const value of item.invalid) expect(() => codec.decode(value)).toThrow();
  }
});

test("portable worker validates extension reads and async output transforms before returning to native", async () => {
  const id = "019a0000-0000-7000-8000-000000000001";
  for (const name of ["transformed", "rejected"]) {
    const requests: unknown[] = [];
    nativeRequest = async operation => {
      const parsed = JSON.parse(operation);
      requests.push(parsed);
      return JSON.stringify(parsed.method === "select" ? [] : { version: "1", row: { id, title: "example", owner: "alice", ordinal: "0" } });
    };
    const call = execute({ ...invocation(`validation.${name}`, resultShape), input_shape: inputShape, input: { title: "  example  " } });
    if (name === "transformed") expect(JSON.parse(await call)).toEqual({ label: "EXAMPLE" });
    else await expect(call).rejects.toThrow("invalid_request");
    expect(requests).toEqual([
      { method: "select", table: "entries", filters: [{ kind: "compare", field: "owner", operator: "=", operand: { kind: "value", value: "alice" } }], limit: null, order: [] },
      { method: "insert", table: "entries", values: { title: "example", owner: "alice" } },
    ]);
  }
});

test("schema comparison ignores object key order but rejects changed contracts and invalid declarations", async () => {
  const request = invocation("boundaries.serialization", resultShape);
  const descriptor = request.schema as Record<string, unknown>;
  request.schema = Object.fromEntries(Object.entries(descriptor).reverse());
  expect(JSON.parse(await execute(request))).toEqual({ label: "safe" });
  await afterSerialization;
  await expect(execute({ ...request, schema: { ...descriptor, uuid_default: "V4" } })).rejects.toThrow("schema_mismatch");
  await expect(execute({ ...request, name: "boundaries.toString" })).rejects.toThrow("invalid_request");
  await expect(execute({ ...request, kind: "query" })).rejects.toThrow("invalid_request");
});

test("result getters cannot schedule database work after handler completion", async () => {
  let calls = 0;
  nativeRequest = async () => { calls++; return "null"; };
  expect(JSON.parse(await execute(invocation("boundaries.serialization", resultShape)))).toEqual({ label: "safe" });
  expect(await afterSerialization).toBeInstanceOf(Error);
  expect(calls).toBe(0);
});

test("unawaited requests and caught native failures cannot produce a successful result", async () => {
  let release: ((value: string) => void) | undefined;
  nativeRequest = () => new Promise(resolve => { release = resolve; });
  try {
    await expect(execute(invocation("boundaries.leaked"))).rejects.toThrow("handler_failed");
  } finally { release?.("null"); }
  nativeRequest = async () => { throw new Error("sensitive native failure detail"); };
  await expect(execute(invocation("boundaries.caught"))).rejects.toThrow("handler_failed");
});

test("a caught affected-row mismatch poisons the invocation before result serialization", async () => {
  nativeRequest = async () => JSON.stringify({ version: "1", row: {
    id: "019a0000-0000-7000-8000-000000000001", title: "count mismatch", owner: "alice", ordinal: "0",
  } });
  await expect(execute(invocation("boundaries.expectation"))).rejects.toMatchObject({ application: JSON.stringify({ code: "row_expectation_failed", message: "Affected-row expectation failed (1 rows)" }) });
});
