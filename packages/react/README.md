# @hulla/db-react

```sh
npm install @hulla/db@0.1.0 @hulla/db-react@0.1.0 react
```

React 19 hooks for generated `@hulla/db` query and mutation functions. Configure
the generated application's browser runtime before mounting components.

```tsx
import { useMutation, useQuery } from "@hulla/db-react";
import { listNotes, saveNote } from "./generated/notes";

function Notes() {
  const notes = useQuery(listNotes, { archived: false });
  const save = useMutation(saveNote);

  return (
    <section>
      {notes.status === "success" && notes.data.map(note => <p key={note.id}>{note.title}</p>)}
      <button onClick={() => { void save({ title: "New note" }); }}>
        {save.state.status === "pending" ? "Saving…" : "Add note"}
      </button>
    </section>
  );
}
```

Pass the generated function to each hook. Queries begin observing after React
subscribes, and equivalent query inputs preserve the observation across renders.
Changing the input releases the previous subscription. Query data is readonly.

`useMutation` returns a callable with a readonly `.state` snapshot. Its state starts
as `idle` and follows the most recently started mutation. Each call returns its
own awaitable mutation handle, including when multiple writes overlap.

For server rendering, pass `{ initialData }` as the query options: after the input
for queries with input, or as the second hook argument for no-input queries. The
seed is read once for the initial query/input; later input changes permanently
discard it, including when returning to the original input. The snapshot remains
`pending` with stale data until a live result arrives. Rendering starts no network
requests. Data rendered for a signed-in account also requires `initialAuthKey`
matching the key supplied to the browser runtime's `setAuth`; anonymous data uses
the default `null` key. A changed authorization scope retires the seed.

`keepPreviousDataOnError: true` preserves the last result for ordinary query
errors. Authorization failures clear it. The shared database runtime owns query
caching, optimistic state and recovery.

## Live pagination

Use `usePaginatedQuery(query, cursor => ({ limit: 20, cursor }))` for a generated
query that directly returns `Page<Row>`. Read the flattened rows through
`tasks.data` and append pages with `tasks.next()` or `tasks.next(3)`.
Changing reactive filters resets all loaded pages; unmounting releases them.
The optional third argument accepts flattened `initialData`, `initialAuthKey`
and the ordinary error-retention option. See the
[pagination guide](../../docs/framework-bindings.md#load-more-pages-in-a-component).

For a complete server-rendered application, see the
[Next.js recipe](../../docs/frameworks/next.md).
