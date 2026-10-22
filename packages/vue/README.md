# @hulla/db-vue

Vue 3 bindings for generated Hulla functions. Install alongside `@hulla/db` and
`vue`, then set `database.client` before generating your app.
The bindings use the core runtime's cache, authorization and subscriptions.

```vue
<script setup lang="ts">
import { useQuery, useMutation } from "@hulla/db-vue";
import { list, save as saveProject } from "./db/_generated/client/projects";

const props = defineProps<{ projectId: string }>();
const projects = useQuery(list, () => ({ projectId: props.projectId }));
const save = useMutation(saveProject);
// await save({ projectId: props.projectId, title: "New title" });
// save.state.value.status is idle, pending, recovering, success or error.
</script>

<template>
  <p v-if="projects.status === 'pending'">Loading…</p>
  <pre v-else-if="projects.status === 'success'">{{ projects.data }}</pre>
</template>
```

Call composables during component setup. `useQuery` returns a readonly shallow
ref of the complete query snapshot. Use `.value` in script; templates unwrap it.
A getter tracks reactive inputs, while equivalent encoded inputs keep the current
subscription. Live observation starts after mount and stops when the scope ends.
No-input queries accept options as their second argument.

For SSR, load data by direct server `db` operations in the application loader. Pass that request's result as `initialData` to `useQuery`, with the same
input and seed on the server and client. It is shown as stale pending data until
live data arrives. For authenticated data, also pass `initialAuthKey` matching
the local key supplied to generated `setAuth` for that account. Input or identity changes
retire the seed. SSR composables never subscribe or issue reads.

```ts
const projects = useQuery(list, () => ({ projectId: props.projectId }), {
  initialData: props.initialProjects,
  initialAuthKey: props.authKey,
});
```

`useMutation` returns a callable mutation with a readonly `.state` shallow ref.
Calls return the original awaitable mutation handle; the state follows the most
recently started call. There is no provider or additional client to construct.

## Live pagination

Use `usePaginatedQuery(query, cursor => ({ limit: 20, cursor }))` for a generated
query that directly returns `Page<Row>`. Read the flattened rows through
`tasks.value.data` and append pages with `tasks.next()` or `tasks.next(3)`.
Changing reactive filters resets all loaded pages; unmounting releases them.
The optional third argument accepts flattened `initialData`, `initialAuthKey`
and the ordinary error-retention option. See the
[pagination guide](../../docs/framework-bindings.md#load-more-pages-in-a-component).

For a complete server-rendered application, see the
[Nuxt recipe](../../docs/frameworks/nuxt.md).
