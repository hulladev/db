import type { mutationBinding, queryBinding } from "../../db/src/callable.ts";
import type { Page, QueryState } from "@hulla/db";
import type { PaginationSnapshot } from "@hulla/db/bindings";
import type { MutationResult } from "../src/index.ts";

export type TitleQuery = ReturnType<typeof queryBinding<{ kind: "query"; $types: { input: { id: number }; output: string } }>>;
export type SaveMutation = ReturnType<typeof mutationBinding<{ kind: "mutation"; $types: { input: string; output: string } }>>;
export interface FixtureProps {
  query: TitleQuery;
  mutation: SaveMutation;
  seed: string;
  ready?: (state: QueryState<string>, save: MutationResult<SaveMutation>, setId: (id: number) => void) => void;
}

export type TasksQuery = ReturnType<typeof queryBinding<{ kind: "query"; $types: { input: { filter: string; cursor: string | null }; output: Page<{ id: number }> } }>>;
export interface PaginationFixtureProps {
  query: TasksQuery;
  seed: readonly { readonly id: number }[];
  ready?: (state: PaginationSnapshot<{ readonly id: number }>, setFilter: (filter: string) => void, tick: () => void) => void;
}
