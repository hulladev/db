# @hulla/db-svelte

```sh
npm install @hulla/db@0.1.0 @hulla/db-svelte@0.1.0 svelte
```

Svelte 5 bindings for generated Hulla functions. Install alongside `@hulla/db`
and `svelte`, then set `database.client` before generation.
The package ships ordinary JavaScript and declarations; consumers do not compile
library rune sources.

```svelte
<script lang="ts">
  import { createQuery, createMutation } from "@hulla/db-svelte";
  import { list, save as saveProject } from "./db/_generated/client/projects";

  let { projectId } = $props<{ projectId: string }>();
  const projects = createQuery(list, () => ({ projectId }));
  const save = createMutation(saveProject);
  // await save({ projectId, title: "New title" });
  // save.state.status is idle, pending, recovering, success or error.
</script>

{#if projects.status === 'pending'}
  <p>Loading…</p>
{:else if projects.status === 'success'}
  <pre>{JSON.stringify(projects.data)}</pre>
{/if}
```

Call bindings during component initialization. Query properties are reactive
getters: read `projects.status`, `projects.data`, `projects.error` and the snapshot
indicators directly, without destructuring them into nonreactive variables.
Getter inputs track rune dependencies. Equivalent encoded inputs retain the
current subscription. Live observation starts on mount and stops on unmount;
SSR never subscribes or executes a read. No-input queries take options as their
second argument. The core runtime owns the cache and authorization.

For SSR, await direct server `db` operations in your server loader,
then pass that request's result to `createQuery` as `initialData`. Supply identical
input and seed on server and client. The result appears as stale pending data
until live data arrives. Authenticated seeds also require `initialAuthKey` equal
to the local key supplied to generated `setAuth` for that account. Input or identity
changes retire the seed.

```ts
const projects = createQuery(list, () => ({ projectId }), {
  initialData: data.initialProjects,
  initialAuthKey: data.authKey,
});
```

`createMutation` returns a callable with a reactive `.state` snapshot getter.
Each call returns its original awaitable mutation handle; `.state` follows the
most recently started call. There is no provider or additional client.

## Live pagination

Use `createPaginatedQuery(query, cursor => ({ limit: 20, cursor }))` for a generated
query that directly returns `Page<Row>`. Read the flattened rows through
`tasks.data` and append pages with `tasks.next()` or `tasks.next(3)`.
Changing reactive filters resets all loaded pages; unmounting releases them.
The optional third argument accepts flattened `initialData`, `initialAuthKey`
and the ordinary error-retention option. See the
[pagination guide](../../docs/framework-bindings.md#load-more-pages-in-a-component).

For a complete server-rendered application, see the
[SvelteKit recipe](../../docs/frameworks/sveltekit.md).
