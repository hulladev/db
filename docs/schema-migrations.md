# Upgrade an application schema

`hulla-db migrate` compares the installed application contract with the schema
in `db.config.ts` and applies supported changes in one PostgreSQL transaction.
Use a provisioning credential. Add `--check` to inspect the plan without changing
storage; there is no confirmation prompt.

```sh
# Optional read-only preview against the target database:
npx hulla-db migrate db.config.ts --check

# During deployment, stop writers and take a backup, then apply:
npx hulla-db migrate db.config.ts
npx hulla-db generate db.config.ts
```

In production, deploy the regenerated application and service together and restart
them. Serving and initialization verify the installed contract; a deployment invokes
`migrate` to upgrade it. A previously connected runtime rejects the changed contract
before executing further application operations. Stop every writer before a production
upgrade; this is a maintenance-window workflow, not rolling mixed-schema deployment.

## Development hot reload

When `hulla-db dev` owns local PostgreSQL, saving a supported schema change regenerates
and validates the code, applies the migration and restarts the service automatically.
Existing local rows persist, including their IDs; nullable/defaulted fields are
backfilled with the same transaction used by the standalone command. Failed generation
or an unsupported migration leaves database contents unchanged and the watcher waits
for corrected source. Development never resets incompatible data.

A configured server URL or `DATABASE_URL` selects external storage. Development does
not initialize or migrate that database automatically. Run `migrate` yourself and
save a source file or restart development after it succeeds. Production `serve` and
ordinary application database imports never perform schema changes.

## Supported changes

- Append tables after the existing tables in the schema declaration. New tables
  may declare indexes, uniqueness/check constraints and references normally.
- Add nullable fields or fields with validated constant defaults to existing tables.
  Existing rows receive the default, or `null` when no default is declared.
- Reorder fields without changing their names or definitions. Physical columns
  are matched by logical field name, including automatically generated IDs.
- Append references; PostgreSQL validates existing values before committing.

For example, adding `done: v.boolean().default(false)` to a populated tasks table
preserves the existing titles and IDs and fills `done` with `false`. Defaults
remain owned by the managed runtime after the backfill; the migration does not
introduce a second database-default implementation.

Removing or renaming tables/fields, reordering existing tables/references,
changing existing types/defaults/nullability/generators, or changing existing
indexes/checks/uniqueness policies is rejected. A required field without a default
is rejected even if the table is empty. Use an explicitly reviewed data
transformation and recovery procedure for changes outside this additive subset;
there is no force flag or implicit fixture reset.

## Atomicity, permissions and recovery

The native runtime binds each plan to the installed contract and deployment source,
then rechecks both under the same advisory lock used by initialization. Metadata
locking drains active application transactions before DDL. A concurrent schema or
source change invalidates the plan. Column changes, backfills, reference validation,
new-table grants, durable quota checks and metadata publication commit together.
Failure leaves the original contract and data usable.

Existing row IDs, scope IDs, deployment source, receipts, admission records and
outbox jobs are preserved. Adding fields increments the affected row versions;
a successful nonempty migration advances each existing scope revision once.
Applying an already-current schema does not advance revisions or repeat a backfill.
Previously committed receipts retain their original results; changing a function's
result contract requires an application-level compatibility decision.

Existing table grants remain attached to their columns/tables. New tables inherit
the first existing application table's owner and table-level privileges. For an
initially empty schema, managed scope read grants and receipt insert grants supply
the corresponding read/runtime-write roles. Review custom per-table or column-level
permissions explicitly; they cannot be inferred for a newly added table.

An interruption during COMMIT may leave the outcome unknown. Never reset or replay
application mutations in response. Inspect `migrate --check` again: a committed upgrade is
already current, while a rolled-back upgrade still has a pending plan. A subsequent
apply validates the current contract again.

Take a schema-scoped backup first using the [backup/restore procedure](production-readiness.md#backup-and-restore-drill).
To revert a successful upgrade, restore the backup into a separate database,
verify it, reapply grants and switch the stopped application back with its matching
artifacts. Writes made after the backup are outside that restore point. The command
does not generate destructive down migrations.

These upgrades concern the application schema within the current published storage
baseline. They do not add readers for unreleased fixtures, wire/file format versions,
or a general storage-format migration framework.

## Validation

Run `make schema-migration-test` with `HULLADB_TEST_URL` pointing at a disposable
PostgreSQL database. It exercises populated-table upgrades, JSON and bigint defaults,
generated IDs and field reordering, reference version triggers, stale sessions/plans,
permission boundaries, quota-triggered rollback and the CLI under Node and Bun.
`make ci-postgres` includes these tests alongside managed transactions, constraints,
recovery, outbox, direct-server and authenticated host integration tests.
Hosted CI also builds the development assets and runs the installed-package hot-reload
fixture against its own PostgreSQL process, including backfills, generation failures
and explicit external-database migrations.
`make backup-test` runs the real backup/restore drill with an explicit privileged
`HULLADB_OPERATIONS_TEST_URL` and matching PostgreSQL client tools.
