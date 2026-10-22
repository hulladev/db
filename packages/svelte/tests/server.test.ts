import { expect, test } from "bun:test";
import { plugin } from "bun";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";
import { createRuntimeScope } from "../../db/src/runtime.ts";
import { mutationBinding, queryBinding } from "../../db/src/callable.ts";

plugin({ name: "svelte-server-fixtures", setup(build) {
  build.onLoad({ filter: /\.svelte$/ }, async ({ path }) => ({
    contents: compile(await Bun.file(path).text(), { generate: "server", filename: path }).js.code, loader: "js",
  }));
} });

test("compiled SSR exposes request-local seeds without network observation", async () => {
  const scope = createRuntimeScope();
  let calls = 0;
  let watches = 0;
  const cleanup = scope.configure(() => ({
    call() { calls++; throw new Error("SSR cannot execute"); },
    watch() { watches++; throw new Error("SSR cannot observe"); },
  }));
  const query = queryBinding<{ kind: "query"; $types: { input: { id: number }; output: string } }>(scope, "title", true);
  const mutation = mutationBinding<{ kind: "mutation"; $types: { input: string; output: string } }>(scope, "save", true);
  try {
    const { default: Fixture } = await import("./Fixture.svelte");
    expect(render(Fixture, { props: { query, mutation, seed: "first" } }).body).toContain("pending:first");
    expect(render(Fixture, { props: { query, mutation, seed: "second" } }).body).toContain("pending:second");
    expect(calls).toBe(0); expect(watches).toBe(0);
  } finally { cleanup(); }
});


test("compiled paginated SSR isolates flattened seeds without starting page observation", async () => {
  const scope = createRuntimeScope();
  let calls = 0;
  let watches = 0;
  const cleanup = scope.configure(() => ({
    call() { calls++; throw new Error("SSR cannot execute"); },
    watch() { watches++; throw new Error("SSR cannot observe"); },
  }));
  const query = queryBinding<{ kind: "query"; $types: { input: { filter: string; cursor: string | null }; output: import("@hulla/db").Page<{ id: number }> } }>(scope, "tasks", true, undefined, true);
  try {
    const { default: Fixture } = await import("./PaginationFixture.svelte");
    expect(render(Fixture, { props: { query, seed: [{ id: 1 }] } }).body).toContain("pending:1:false");
    expect(render(Fixture, { props: { query, seed: [{ id: 2 }] } }).body).toContain("pending:2:false");
    expect(calls).toBe(0); expect(watches).toBe(0);
  } finally { cleanup(); }
});
