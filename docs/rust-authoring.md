# Rust authoring and direct calls

`hulladb` provides the native Rust authoring surface over
[managed PostgreSQL functions](managed-functions.md). `hulladb_macros` emits
canonical table/input metadata, checked codecs, typed field selectors, insert and
patch inputs, function registration adapters, direct async calls and query watches.
The [tasks example](../crates/hulladb/examples/tasks.rs) compiles with the workspace
and runs against an owned PostgreSQL namespace:

```sh
make db-up
cargo run -p hulladb --example tasks --locked
make rust-authoring-test
```

## Tables and inputs

```rust
use hulladb::{Id, Input, Table};

#[derive(Table)]
#[table(name = "tasks")]
pub struct Task {
    pub id: Id<Task>,
    pub title: String,
    #[field(default = false)]
    pub completed: bool,
    pub owner: String,
}

#[derive(Input)]
pub struct CreateTask {
    pub title: String,
}

let contract = hulladb::schema!(Task)?;
```

Authored structs must have named fields and no generic parameters. Supported
scalars are `String`, `bool`, `i64`, `Uuid`, `Id<Table>` and `Option` of those.
The dependency must be named `hulladb`. `Id<Task>` wraps Task's key type, so IDs
from unrelated tables cannot be substituted even when their scalar keys match.
Rows explicitly declare their primary key; the default field is `id`, and
`#[table(key = "slug")]` selects another Rust field. A primitive key uses its
authored scalar type; a branded key defaults to UUID, with `key_type = i64` or
`key_type = String` available for other branded keys.

`schema!` normalizes the declarations through `hulladb_schema`, preserving the
existing constraint validation. UUID keys inherit UUIDv7 by default. A declaration
can select `generation = "uuidV4"`, `"uuidV7"` or `"none"`, or a named custom
`generator = "counters.next"` registered in the managed registry. Primitive
non-UUID keys otherwise require an explicit value. Key policies use
`insert_key_policy = "allowExplicit"` (default) or `"generatedOnly"`.

`Id<OtherTable>` on a non-key field emits a reference to that table; the target
must be included in the schema. Primitive reference fields can instead use
`#[field(reference = OtherTable)]`. `#[field(name = "ownerId")]` changes the
canonical/wire field name while keeping Rust field access unchanged. Defaults
accept string, Boolean, signed integer and hyphenated UUID literals, or `None`
for nullable fields. A scalar default on `Option<T>` means a non-null default.
Expressions, runtime callbacks, nested records and arbitrary Rust types are outside
the derive subset.

Named unique constraints use Rust field identifiers, with explicit null behavior
for nullable fields:

```rust
#[unique(name = "owner_title", fields(owner, title))]
#[unique(name = "optional_code", fields(code), nulls = "distinct")]
// Applied above a #[derive(Table)] struct containing these fields.
```

Each table emits `TaskFields`, `TaskInsert` and `TaskPatch`. Ordinary inserts omit
generated keys. Defaulted insert fields are optional: `None` omits the field and
lets storage apply its default. If explicit generated keys are allowed,
`TaskInsertWithKey` adds an optional key. Every patch field is optional and the
key is excluded. For a nullable field, `None` means no patch,
`Some(None)` clears the field and `Some(Some(value))` sets it. Nullable fields
without defaults remain required in ordinary inserts and derived function inputs.

Table row decoding requires all fields, even defaulted ones. Input decoding can
apply declared defaults. Both reject unknown fields and invalid values. UUIDs
use hyphenated strings and int64 values use canonical decimal strings on the wire;
JSON numbers cannot silently lose integer precision. These match the deployed
TypeScript function codecs.

## Functions and typed contexts

```rust
use hulladb::{mutation, query, Error, MutationContext, QueryContext};

#[query(name = "tasks.pending")]
pub async fn pending(ctx: QueryContext<'_, String>) -> Result<Vec<Task>, Error> {
    ctx.db.select::<Task>()
        .where_eq(Task::fields().owner, ctx.identity.clone())
        .where_eq(Task::fields().completed, false)
        .take(50).await
}

#[mutation(name = "tasks.create")]
pub async fn create(
    ctx: MutationContext<'_, String>, input: CreateTask,
) -> Result<Task, Error> {
    ctx.db.insert(TaskInsert {
        title: input.title,
        completed: None,
        owner: ctx.identity.clone(),
    }).await
}
```

The identity type is application-defined and defaults to `()`. Attributes accept
an ordinary async function with its context and optionally one owned input. They
emit checked registration adapters, preserve the function's visibility, and replace
its public call signature with the input-only async binding. An explicit operation
name gives deployed clients a stable name; the default combines `module_path!()`
and the function name. Results support the scalar codecs, derived table/input
structs, `Option<T>`, `Vec<T>` and `()`; custom result structs can derive `Input`
within the same scalar-field subset or implement `Codec` explicitly.

Query contexts expose only reads in the managed snapshot. Mutation contexts expose
reads and writes inside one managed transaction. Typed predicates require both the
correct table and value type. `select::<Task>().await` returns logical rows;
`.versioned().await` returns `Stored<Task>` with a separate storage version.
`get(id)` also returns an optional `Stored<Task>`. `insert(input)` returns a row,
and `insert_versioned(input)` returns row plus version. `update(id, version, patch)`
and `delete(id, version)` require the optimistic storage version and return the row.
These accessors compare derived metadata with the installed canonical contract
before executing, including references, defaults, generation and constraints.

The attributes emit `pending::register(&mut registry, policy)` and
`create::register(&mut registry, policy)` for the existing `Registry<Identity>`.
Authorization is mandatory and implements `hulladb::Authorization<Identity>`.
Its typed read-only context tracks policy reads in the same transaction. Receipt
confirmation delivers the original authorized response without rerunning policy,
handler or validators. The host verifies current credentials, and storage checks
the original principal, data/authentication scope, source and expiry. Register these operations in the
[generic HTTP/SSE host](sdk-adapters.md) to expose them to generated TypeScript
clients without per-operation routes or hand-written value codecs.

Registered callable bindings cannot nest inside a handler or its policy. They
fail with `InvalidInput`, preventing connection reentry and transaction deadlocks.
Compose server behavior using ordinary helpers and the existing context database,
so all writes remain inside the same transaction.

## Direct calls and owned watches

Create a `LocalRuntime` with an installed `ManagedSchema`, registered functions and
a verified `Context<Identity>`, then establish an application/request task scope:

```rust
// runtime: Arc<LocalRuntime<String>>, initialized once for this verified identity.
hulladb::scope(runtime.clone(), async {
    let task = create(CreateTask { title: "Ship it".into() }).await?;
    let tasks = pending().await?;
    let mut subscription = pending::watch().await?;
    subscription.changed().await?;
    // subscription.state(): Loading, Ready { data, source, revision, connected }, or Error.
    drop(subscription);
    Ok::<_, hulladb::Error>(())
}).await?;
```

Callers import `create`, `pending` and public input types from the authored module;
they need no client method or `run` wrapper. Each function namespace exports `NAME`,
`Input` and `Output`; server builds also export `register`, queries export `watch`,
and mutations export `contract()` codecs for durable remote recovery. Mutations
cannot be watched. Calls outside a scope fail. Tokio child tasks do not inherit the scope;
establish one explicitly when spawning work. Concurrent scopes bind separate,
immutable identities. Retire the old runtime before switching identity; retirement
rejects further calls and clears owned watch state. Dropping a subscription aborts
its worker.

`LocalRuntime` is for trusted native jobs and embedded/server use. It verifies
existing storage and fixes the deployment source at connection time. It owns a
checked PostgreSQL connection, polls registered queries, and validates decoded
results. Watches stop with an error on authorization/transport failure; automatic
remote reconnect and resubscription are not provided by this runtime. A custom
`Runtime` can provide another transport while retaining the same callable API.

Mutations journal a request ID, input and deployment source before submission.
Cancellation or an unknown commit outcome keeps the intent in memory.
`recover_pending()` looks up a receipt first and otherwise replays the same request
with current authorization, avoiding duplicate acknowledged writes.
`pending()` exposes retained intents. The journal does not survive process exit,
and a retired identity cannot recover through its retired runtime. The optional [Rust remote client](rust-remote-client.md) provides generic native
HTTP/SSE, SQLite result/journal persistence and client-only macro emission. Use
`default-features = false, features = ["remote"]` to omit PostgreSQL and server
registration. Both runtimes retain the same direct-call and watch API; remote
watches also expose connection freshness and receipt visibility waiting.

Cross-language generation from Rust function signatures and framework-specific
authentication providers are separate work. TypeScript handlers can execute through
the optional [transaction worker](typescript-functions.md).
Typed native admin transactions and Node/Bun request adapters are described in
[production operations](production-readiness.md). No compatibility versions or upgrade migrations are added.

## Serializable mutation registration

Generated mutation modules also expose `register_with_concurrency`, using the same
policy adapter and contracts as ordinary registration. `MutationConcurrency::Serializable`
accepts `RetryOptions` for attempts, total deadline and backoff. More than one attempt
explicitly declares the complete handler/policy/generator path safe to replay after
a confirmed rollback. Expected-version conflicts and unknown commit outcomes are
never internally replayed. See [transaction concurrency](concurrency-and-optimism.md).
