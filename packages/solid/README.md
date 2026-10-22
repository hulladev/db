# @hulla/db-solid

Solid bindings for generated Hulla functions. Install alongside `@hulla/db` and
`solid-js`, then set `database.client` before generation.

```tsx
import { createQuery, createMutation } from "@hulla/db-solid";
import { list, save as saveProject } from "./db/_generated/client/projects";

function Projects(props: { projectId: string }) {
  const projects = createQuery(list, () => ({ projectId: props.projectId }));
  const save = createMutation(saveProject);
  // await save({ projectId: props.projectId, title: "New title" });
  // save.state().status is idle, pending, recovering, success or error.
  return <pre>{JSON.stringify(projects().data)}</pre>;
}
```

Call bindings in a component owner. `createQuery` returns an accessor containing
the complete query snapshot. Getter inputs follow Solid signals; equivalent
encoded inputs keep the current subscription. Observation starts after mount and
stops with owner cleanup. No-input queries accept options as their second argument.
The core database runtime owns caching, authorization and subscriptions.

For SSR, await direct server `db` operations in the application loader,
then supply the request's result as `initialData` with identical input and seed
on server and client. The seed appears as stale pending data until live data
arrives. Authenticated seeds also need `initialAuthKey` matching the local key
supplied to generated `setAuth` for that account. Input or identity changes retire the
seed. Server bindings never subscribe or issue reads.

```ts
const projects = createQuery(list, () => ({ projectId: props.projectId }), {
  initialData: props.initialProjects,
  initialAuthKey: props.authKey,
});
```

`createMutation` returns a callable mutation with a `.state` accessor. Each call
returns its original awaitable mutation handle; the state tracks the most recently
started call. No provider or additional client is needed.

## Live pagination

Use `createPaginatedQuery(query, cursor => ({ limit: 20, cursor }))` for a generated
query that directly returns `Page<Row>`. Read the flattened rows through
`tasks().data` and append pages with `tasks.next()` or `tasks.next(3)`.
Changing reactive filters resets all loaded pages; unmounting releases them.
The optional third argument accepts flattened `initialData`, `initialAuthKey`
and the ordinary error-retention option. See the
[pagination guide](../../docs/framework-bindings.md#load-more-pages-in-a-component).

For a complete server-rendered application, see the
[SolidStart recipe](../../docs/frameworks/solidstart.md).
