import { expect, test } from "bun:test";
import { createRoot } from "solid-js";
import { createRuntimeScope } from "../../db/src/runtime.ts";
import { mutationBinding, queryBinding } from "../../db/src/callable.ts";
import { type Page } from "@hulla/db";
import { createMutation, createPaginatedQuery, createQuery } from "../src/index.ts";

test("server owners expose request-local seeds without starting a runtime", () => {
  const scope = createRuntimeScope();
  const cleanup = scope.configure(() => {
    return { call() { throw new Error("SSR cannot execute declarations"); }, watch() { throw new Error("SSR cannot watch"); } };
  });
  const query = queryBinding<{ kind: "query"; $types: { input: undefined; output: string } }>(scope, "title", false);
  const mutation = mutationBinding<{ kind: "mutation"; $types: { input: undefined; output: string } }>(scope, "save", false);
  try {
    for (const seed of ["first", "second"]) createRoot(dispose => {
      const state = createQuery(query, { initialData: seed });
      expect(state()).toEqual({ status: "pending", data: seed, isStale: true, isOptimistic: false });
      expect(createMutation(mutation).state()).toEqual({ status: "idle" });
      dispose();
    });
  } finally { cleanup(); }
});


test("server paginated owners expose flattened request-local seeds without observation", () => {
  const scope = createRuntimeScope();
  const cleanup = scope.configure(() => ({ call() { throw new Error("SSR cannot execute"); }, watch() { throw new Error("SSR cannot observe"); } }));
  const query = queryBinding<{ kind: "query"; $types: { input: { cursor: string | null }; output: Page<{ id: number }> } }>(scope, "tasks", true, undefined, true);
  try {
    for (const id of [1, 2]) createRoot(dispose => {
      const state = createPaginatedQuery(query, cursor => ({ cursor }), { initialData: [{ id }] });
      expect(state().status).toBe("pending");
      expect(state().data).toEqual([{ id }]);
      expect(state().hasMore).toBe(false);
      dispose();
    });
  } finally { cleanup(); }
});
