import { createRuntimeScope } from "../../db/src/runtime.ts";
import { mutationBinding, queryBinding } from "../../db/src/callable.ts";
import { useMutation, useQuery, usePaginatedQuery } from "../src/index.ts";

const scope = createRuntimeScope();
const query = queryBinding<{ kind: "query"; $types: { input: { id: number }; output: { title: string; tags: string[] } } }>(scope, "note", true);
const noInputQuery = queryBinding<{ kind: "query"; $types: { input: undefined; output: string } }>(scope, "title", false);
const mutation = mutationBinding<{ kind: "mutation"; $types: { input: { title: string }; output: { id: number } } }>(scope, "save", true);
const noInputMutation = mutationBinding<{ kind: "mutation"; $types: { input: undefined; output: number } }>(scope, "reset", false);

// Checked by tsc: generated contracts must survive the framework boundary.
export function BindingTypes() {
  const state = useQuery(query, { id: 1 }, { initialData: { title: "seed", tags: [] } });
  if (state.status === "success") {
    const title: string = state.data.title;
    // @ts-expect-error Query data remains deeply readonly.
    state.data.tags.push(title);
  }
  useQuery(noInputQuery);
  useQuery(noInputQuery, { initialData: "seed", initialAuthKey: "alice" });
  // @ts-expect-error Query input is required.
  useQuery(query);
  // @ts-expect-error Query input keeps its generated type.
  useQuery(query, { id: "wrong" });
  // @ts-expect-error Initial data does not widen the generated output.
  useQuery(query, { id: 1 }, { initialData: "wrong" });
  // @ts-expect-error Mutation functions cannot be queried.
  useQuery(mutation, { title: "wrong" });
  const save = useMutation(mutation);
  const outcome: PromiseLike<{ id: number }> = save({ title: "note" });
  // @ts-expect-error Mutation inputs stay required.
  save();
  // @ts-expect-error Mutation snapshots are readonly.
  save.state = { status: "idle" };
  useMutation(noInputMutation)();
  // @ts-expect-error No-input mutations reject accidental options or input.
  useMutation(noInputMutation)({});
  // @ts-expect-error Queries cannot be mutated.
  useMutation(noInputQuery);
  return outcome;
}

const paginatedQuery = queryBinding<{ kind: "query"; $types: { input: { limit: number; cursor: string | null }; output: import("@hulla/db").Page<{ title: string; tags: string[] }> } }>(scope, "pages", true, undefined, true);
const plainPageQuery = queryBinding<{ kind: "query"; $types: { input: { cursor: string | null }; output: { page: string[]; cursor: string | null } } }>(scope, "plain", true);
export function PaginatedBindingTypes() {
  const state = usePaginatedQuery(paginatedQuery, cursor => ({ limit: 10, cursor }), { initialData: [{ title: "seed", tags: [] }] });
  if (state.status === "success") {
    const title: string = state.data[0]!.title;
    // @ts-expect-error Page rows preserve deeply readonly query results.
    state.data[0]!.tags.push(title);
    // @ts-expect-error Flattened lists are readonly.
    state.data.push({ title, tags: [] });
  }
  const loading: Promise<void> = state.next(2);
  // @ts-expect-error The cursor mapper preserves generated input requirements.
  usePaginatedQuery(paginatedQuery, cursor => ({ cursor }));
  // @ts-expect-error The cursor mapper preserves generated input value types.
  usePaginatedQuery(paginatedQuery, cursor => ({ cursor, limit: "wrong" }));
  // @ts-expect-error Seeds cannot widen the page row type.
  usePaginatedQuery(paginatedQuery, cursor => ({ cursor, limit: 10 }), { initialData: ["wrong"] });
  // @ts-expect-error Ordinary queries do not carry the page capability.
  usePaginatedQuery(noInputQuery, () => undefined);
  // @ts-expect-error Similar output shape does not confer eligibility.
  usePaginatedQuery(plainPageQuery, cursor => ({ cursor }));
  // @ts-expect-error Mutation functions cannot be paginated.
  usePaginatedQuery(mutation, () => ({ title: "wrong" }));
  return loading;
}
