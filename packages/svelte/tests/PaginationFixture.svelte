<script lang="ts">
  import { createPaginatedQuery } from "../src/index.ts";
  import type { PaginationFixtureProps } from "./fixture-types.ts";
  let { query, seed, ready }: PaginationFixtureProps = $props();
  let filter = $state("open");
  let tick = $state(0);
  const result = createPaginatedQuery(query, cursor => { tick; return { filter, cursor }; }, { initialData: seed });
  ready?.(result, next => { filter = next; }, () => { tick++; });
</script>
<p>{result.status}:{result.data?.map(row => row.id).join(",") ?? ""}:{result.hasMore}</p>
