import { paginated as paginatedEntries } from "./generated/deployed.ts";
import { defineFunctions } from "../src/declarations.ts";
import { v } from "../src/schema.ts";
import { objectInput, standard } from "./fixtures/standard.ts";
import type { Input, Output } from "../src/index.ts";
import type { MutationCallable, QueryCallable } from "../src/callable.ts";
import type { MutationHandle, QueryHandle, ReadonlyResult } from "../src/contracts.ts";
import { create, pending } from "./generated/notes.ts";
import type { Note, ReadContext, WriteContext } from "./fixtures/server.ts";

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends (<T>() => T extends B ? 1 : 2) ? true : false;
function expectType<T extends true>(_value: T): void {}

// Compile-time contract regressions, never executed.
async function contracts() {
  expectType<Equal<Input<typeof pending>, undefined>>(true);
  expectType<Equal<Output<typeof pending>, readonly Readonly<Note>[]>>(true);
  expectType<Equal<Input<typeof create>, { title: string }>>(true);
  expectType<Equal<Output<typeof create>, Note>>(true);
  const notes: QueryHandle<Note[]> = pending();
  const created: MutationHandle<Note> = create({ title: "Valid" });
  const rows: ReadonlyResult<Note[]> = await notes;
  // @ts-expect-error awaited query arrays cannot be edited
  rows.push({ id: 1, title: "Changed", owner: "alice" });
  // @ts-expect-error query rows must be copied before editing
  rows[0]!.title = "Changed";
  // @ts-expect-error public mutation modes are removed
  create({ title: "Predicted" }, { mode: "optimistic" });
  // @ts-expect-error mutations cannot be cancelled
  create({ title: "Invalid" }, { signal: AbortSignal.abort() });
  // @ts-expect-error mutation modes cannot be passed to query bindings
  pending({ mode: "optimistic" });
  // @ts-expect-error subscriptions receive query options only
  pending.subscribe(() => {}, { mode: "optimistic" });
  const optimistic = create.withOptimistic({
    query: pending,
    update: (current, input, queryInput) => {
      const noInput: undefined = queryInput;
      void noInput;
      // @ts-expect-error optimistic updaters receive readonly cached query arrays
      current.push({ id: 1, owner: "alice", title: "Changed" });
      // @ts-expect-error mutation inputs stay typed in optimistic callbacks
      input.title = 1;
      return [...current, { id: 1, title: input.title, owner: "alice" }];
    },
  });
  expectType<Equal<Input<typeof optimistic>, Input<typeof create>>>(true);
  expectType<Equal<Output<typeof optimistic>, Output<typeof create>>>(true);
  const byOwner = null as unknown as QueryCallable<{ kind: "query"; $types: { input: { owner: string }; output: number } }>;
  create.withOptimistic({ query: pending, update: current => current }, {
    query: byOwner,
    match: (queryInput, input) => queryInput.owner === input.title,
    update: (current, input, queryInput) => {
      current.toFixed();
      input.title.toUpperCase();
      queryInput.owner.toUpperCase();
      // @ts-expect-error separate target result types do not widen to one union
      current.push({});
      return current + 1;
    },
  });
  create.withOptimistic({
    query: byOwner,
    // @ts-expect-error query input mapping must match the target query
    input: () => ({ owner: 1 }),
    update: current => current,
  });
  create.withOptimistic({ query: byOwner, input: input => ({ owner: input.title }), update: current => current });
  // @ts-expect-error input-bearing queries need an exact input mapping or a match predicate
  create.withOptimistic({ query: byOwner, update: current => current });
  create.withOptimistic({
    query: byOwner,
    // @ts-expect-error match predicates receive the target query's input
    match: queryInput => queryInput.missing,
    update: current => current,
  });
  // @ts-expect-error optimistic wrappers need at least one target
  create.withOptimistic();
  // @ts-expect-error mutation bindings cannot be optimistic query targets
  create.withOptimistic({ query: create, input: () => undefined, update: current => current });
  const saved: MutationHandle<Note> = optimistic({ title: "Optimistic" });
  void saved;
  const namespace = { tasks: { pending, create } };
  namespace.tasks.pending.subscribe((state) => {
    if (state.status === "success") {
      state.data[0]?.owner.toUpperCase();
      // @ts-expect-error subscription rows cannot be edited
      state.data[0]!.owner = "bob";
    }
  }, { keepPreviousDataOnError: true });
  notes.subscribe(state => {
    // @ts-expect-error queries never expose mutation recovery
    if (state.status === "recovering") return;
    if (state.status === "success") state.data[0]?.title.toLowerCase();
  });
  created.subscribe(state => {
    if (state.status === "success") state.data.owner.toUpperCase();
  });
  // @ts-expect-error mutation inputs are inferred from validators
  create({ title: 1 });
  // @ts-expect-error input is required
  create();
  // @ts-expect-error mutation callables are observed through their invocation handles
  create.subscribe(() => {});
  // @ts-expect-error superseded watch API is removed
  pending.watch(() => {});
  // @ts-expect-error handles have no separate result accessor
  notes.result;
  const nested = null as unknown as QueryHandle<{ nested: { list: { title: string }[] }; when: Date }>;
  const nestedResult = await nested;
  // @ts-expect-error readonly applies to nested ordinary arrays
  nestedResult.nested.list.push({ title: "New" });
  // @ts-expect-error readonly applies to nested ordinary objects
  nestedResult.nested.list[0]!.title = "New";
  nestedResult.when.getTime();
  const { query, mutation } = defineFunctions<ReadContext, WriteContext>();
  const noInputQuery = query(() => [] as Note[]);
  const noInputMutation = mutation(() => [] as Note[]);
  const noInputMutationBinding = null as unknown as MutationCallable<typeof noInputMutation>;
  expectType<Equal<Input<typeof noInputQuery>, undefined>>(true);
  expectType<Equal<Output<typeof noInputQuery>, Output<typeof pending>>>(true);
  expectType<Equal<Input<typeof noInputMutation>, undefined>>(true);
  expectType<Equal<Input<typeof noInputMutationBinding>, undefined>>(true);
  expectType<Equal<Output<typeof noInputMutationBinding>, Note[]>>(true);
  const noInputOptimistic = noInputMutationBinding.withOptimistic({ query: pending, update: current => current });
  expectType<Equal<Input<typeof noInputOptimistic>, undefined>>(true);
  expectType<Equal<Output<typeof noInputOptimistic>, Note[]>>(true);
  query(({ db }) => {
    // @ts-expect-error queries do not receive write access
    return db.insert("Forbidden", "alice");
  });
  mutation({ input: objectInput({ title: v.string() }), handler: ({ db }, { title }) => db.insert(title, "alice") });
  query({ limits: { inputBytes: 1024, executionMs: 10000 }, handler: () => 1 });
  // @ts-expect-error limits belong to final declarations, not reusable extensions
  query.extend({ limits: { inputBytes: 1024 }, handler: () => ({}) });
  const shared = query.extend({
    input: standard<{ page: string }, { page: number }>(() => ({ page: 1 })),
    handler: (ctx, input) => ({ pageLabel: `${ctx.identity}:${input.page.toFixed()}` }),
  });
  const shaped = shared({
    input: standard<{ limit: number }, { limit: number }>(() => ({ limit: 10 })),
    output: standard<number, { label: string }>(() => ({ label: "result" })),
    handler: (ctx, input) => {
      ctx.pageLabel.toUpperCase();
      input.page.toFixed();
      input.limit.toFixed();
      // @ts-expect-error handlers see parsed page values rather than submitted strings
      input.page.toUpperCase();
      return input.page + input.limit;
    },
  });
  const raw: Input<typeof shaped> = { page: "2", limit: 10 };
  const output: Output<typeof shaped> = { label: "result" };
  const shapedBinding = null as unknown as QueryCallable<typeof shaped>;
  expectType<Equal<Input<typeof shapedBinding>, { page: string } & { limit: number }>>(true);
  expectType<Equal<Output<typeof shapedBinding>, { readonly label: string }>>(true);
  expectType<Equal<Output<typeof shapedBinding>, Output<typeof shaped>>>(true);
  // @ts-expect-error inferred query outputs preserve the readonly query contract
  output.label = "changed";
  const transformedMutation = mutation({
    input: standard<{ title: string }, { title: number }>(() => ({ title: 1 })),
    output: standard<number, { label: string }>(() => ({ label: "result" })),
    handler: (_ctx, input) => input.title,
  });
  const transformedMutationBinding = null as unknown as MutationCallable<typeof transformedMutation>;
  expectType<Equal<Input<typeof transformedMutation>, { title: string }>>(true);
  expectType<Equal<Input<typeof transformedMutationBinding>, { title: string }>>(true);
  expectType<Equal<Output<typeof transformedMutationBinding>, { label: string }>>(true);
  expectType<Equal<Output<typeof transformedMutationBinding>, Output<typeof transformedMutation>>>(true);
  void raw; void output;
  // @ts-expect-error public inputs retain the original raw page type
  const parsedAsRaw: Input<typeof shaped> = { page: 2, limit: 10 };
  void parsedAsRaw;
  // @ts-expect-error output schema determines public result type
  const untransformedOutput: Output<typeof shaped> = 2;
  void untransformedOutput;
  // @ts-expect-error earlier computed context keys cannot be overwritten
  shared.extend(() => ({ pageLabel: "replacement" }));
  // @ts-expect-error core capabilities cannot be replaced by an extension
  query.extend(() => ({ db: {} }));
  // @ts-expect-error identity cannot be forged by an extension
  query.extend(() => ({ identity: "forged" }));
  // @ts-expect-error prototype keys cannot be introduced by extensions
  query.extend(() => ({ constructor: "forged" }));
  // @ts-expect-error scalar extension results cannot add context properties
  query.extend(() => 2);
  // @ts-expect-error arrays cannot add context properties
  query.extend(() => [1, 2]);
  // @ts-expect-error known duplicate parsed keys fail even when their types match
  shared({ input: standard<{ other: string }, { page: number }>(() => ({ page: 2 })), handler: () => 1 });
  const arrayInput = query.extend({ input: standard<string[], string[]>(() => []), handler: () => {} });
  // @ts-expect-error multiple parsed input schemas cannot merge arrays into an object
  arrayInput({ input: standard<{ tag: string }>(() => ({ tag: "x" })), handler: () => 1 });
  const objectInputBase = query.extend({ input: standard<{ tag: string }>(() => ({ tag: "x" })), handler: () => {} });
  // @ts-expect-error multiple parsed input schemas cannot merge an object and array
  objectInputBase({ input: standard<string[], string[]>(() => []), handler: () => 1 });
  query({ input: standard<string[], string[]>(() => []), handler: (_ctx, values) => values.length });
  // @ts-expect-error an output schema constrains the successful handler return type
  query({ output: standard<number, string>(() => "text"), handler: () => "wrong input for output schema" });
  // @ts-expect-error superseded positional validator declarations are removed
  mutation({ title: v.string() }, () => 1);
  // @ts-expect-error function declarations require Standard Schema rather than database validators
  query({ input: v.string(), handler: () => 1 });

  return { notes, created };
}
void contracts;

function generatedPagination() {
  const entries = paginatedEntries.paginate(cursor => ({ limit: 20, cursor }));
  entries.subscribe(state => { if (state.data) state.data[0]?.title.toUpperCase(); });
  // @ts-expect-error ordinary generated queries do not expose pagination
  pending.paginate(() => undefined);
  // @ts-expect-error generated mutations do not expose pagination
  create.paginate(() => ({ title: "x" }));
}
void generatedPagination;
