<script lang="ts">
  import { createQuery, createMutation } from "../src/index.ts";
  import type { FixtureProps } from "./fixture-types.ts";
  let { query, mutation, seed, ready }: FixtureProps = $props();
  let input = $state({ id: 1 });
  const result = createQuery(query, () => ({ id: input.id }), { initialData: seed });
  const save = createMutation(mutation);
  ready?.(result, save, next => { input = { id: next }; });
</script>
<p>{result.status}:{result.data ?? ""}</p><span>{save.state.status}</span>
