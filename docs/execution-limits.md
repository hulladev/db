# Configure execution limits

Use `limits` in `db.config.ts` to set server defaults. A final query/mutation can
override individual values; omitted values inherit the config. Limits belong to
trusted declarations, never browser input or `.extend(...)` stages.

```ts
export default defineConfig({
  database: { server: { url: process.env.DATABASE_URL! } },
  limits: { executionMs: 10_000, outputBytes: 24 * 1024 * 1024 },
})
```

```ts
export const summary = query({
  limits: { executionMs: 2000, readRows: 5000 },
  handler: ctx => ctx.db.select('orders').aggregate(a => ({ count: a.count() })),
})
```

Regenerate after changing defaults or declaration overrides: the service loads
the effective configuration from generated artifacts. Generated direct `db` handles
inherit config limits; a server transaction shares one budget across its statements.

| Setting | Default | Current maximum |
| --- | --- | --- |
| `executionMs` | 5000 | 60,000 |
| `inputBytes` | 16 MiB | 64 MiB |
| `outputBytes` | 16 MiB | 64 MiB |
| `memoryBytes` | 64 MiB | 1 GiB |
| `stackBytes` | 512 KiB | 16 MiB |
| `operations` | 1000 | 1,000,000 |
| `readRows` | 100,000 | 1,000,000 |
| `readBytes` | 64 MiB | 64 MiB |

Settings must be positive safe integers. The maxima bound the current process and
transport implementation; they are validated when config/declarations load.
Raising a payload budget may also require a larger worker heap, since decoded
values, validation and output transformation use memory independently.

Input/output bytes count uncompressed JSON-encoded function values without their
protocol envelopes. Input is checked before input validators, extensions and the
handler. Output is checked after output transformation and before mutation commit.
Each live result has its own output check, rather than a lifetime byte allowance.
HTTP and native framing reserve room for envelopes and honor raised payload limits.

Execution time covers server processing, including handler setup, SQL and output
validation. It excludes browser upload and response delivery. Declared mutation
retries still share their enclosing transaction deadline. Client request timeouts,
connection recovery and mutation confirmation are separate budgets; cancelling or
timing out communication does not prove that a mutation rolled back.

Configure browser deadlines separately when an operation needs longer than the
transport defaults:

```ts
// Inside db.config.ts: database.client
const client = {
  url: 'https://database.example.com',
  operationTimeoutMs: 60_000,
  queryTimeoutMs: 90_000,
  confirmationTimeoutMs: 90_000,
  streamIdleTimeoutMs: 90_000,
}
```

`operationTimeoutMs` defaults to 10 seconds per request. `queryTimeoutMs` defaults
to 30 seconds overall for a one-shot read, including session setup and transport
retries; server errors are terminal. Mutation confirmation defaults to 30 seconds
and only checks the original submission after a lost response. Stream idleness
defaults to 30 seconds without activity. Generation also sizes response
framing from the server's effective payload budgets; low-level `HttpRuntime` users
must set `maxResponseBytes` themselves when raising those budgets.

Fetched-data budgets count rows actually returned by database operations and their
PostgreSQL binary column payload bytes. They include write `RETURNING`, storage
versions and private pagination fields. Streaming checks stop collection when a
budget is exceeded, and a failed budget prevents commit even when caught. They do
not measure index traversal or rows scanned inside PostgreSQL. One row must first
reach the driver before it can be charged; these settings are not a hard native
process RSS limit. The worker's JavaScript heap is bounded separately.

`operations` counts database requests in one invocation/transaction. An aggregate
is one request; it does not make a large SQL scan cheap. PostgreSQL statement and
lock timeouts also bound database execution. Coordinated live delivery retains an
additional combined round/frame bound, separate from each function's output limit.

Outside production, successful executions warn at 80% of measured effective time,
payload, operation or fetched-data budgets. Messages name the budget and usage;
they do not log SQL, submitted values or results. Stack usage is not sampled.

Worker heap/stack settings apply to restricted query/mutation handlers. Ordinary
application-server callbacks run in the application's JavaScript runtime; the
database package cannot cap that runtime's heap or forcibly cancel its JavaScript
callback. Expired database transaction handles are fenced and cannot commit.
