# Agent guidance

## Validation scope

- Choose validation from the actual diff and plausible failure modes. Run only
  relevant CI jobs or local equivalents, when available. Do not default to
  `make check`, `cargo test --workspace`, or every database/benchmark suite at the
  end of each task or session.
- Start with the affected test cases or integration test target. Expand to the
  affected crate and relevant consumers when a shared contract changes. Consider
  runtime dependencies and test dependencies; a change in one crate can affect
  another crate's integration tests.
- Run the full suite when regressions could be widespread, such as changes to
  workspace/toolchain configuration, broadly used dependencies, shared contracts
  across multiple crates, or broad refactors. Explain the reason briefly.
- Once relevant checks pass, do not repeat them unless subsequent edits, new
  failures, or unresolved concerns justify it. Reuse earlier results only when
  the checked code and relevant environment have not changed.
- Prose-only documentation and agent-instruction edits normally need diff review,
  not application tests. Validate executable examples or tooling if those change.
- Summarize what ran and its result, plus relevant checks that could not run.
  Keep successful output concise; include diagnostic detail for failures. Never
  imply an ignored or unrun integration test passed.

## Choosing local checks

There are currently no hosted CI workflows in this repo. The Makefile provides
local checks; `make check` is the broad validation entry point, not the default
for every edit. Use the following commands as examples, selecting only what the
change warrants:

- Focused Rust tests:
  `cargo test -p <crate> --test <target> --locked [test_filter]`.
  Use `--lib` for unit tests or `--doc` for executable documentation when relevant.
- Broader crate tests: `cargo test -p <crate> --locked`.
- Rust formatting and linting for affected crates:
  `cargo fmt -p <crate> -- --check` and
  `cargo clippy -p <crate> --all-targets --locked -- -D warnings`.
- PostgreSQL integration tests are ignored in ordinary Cargo runs. For affected
  database behavior, select the relevant target and optionally a test filter:
  `cargo test -p hulladb_postgres --test task_store --locked -- --ignored`, or
  `cargo test -p hulladb_cdc --test <target> --locked -- --ignored --test-threads=1`
  where the CDC target is `postgres_capture`, `postgres_snapshot`, or `command_sync`.
  These require PostgreSQL; see the README for setup. Use `make db-test` or
  `make cdc-test` when the whole corresponding integration suite is relevant.
- Benchmark runner changes: run relevant Python cases in
  `scripts/tests/test_bench.py`; use `make bench-check` when the runner's behavior
  is broadly affected. Run performance benchmarks only when the change warrants
  performance validation, choosing the affected suite.

## Test quality

- Keep test functions in each crate's `tests/` directory. Prefer integration test
  targets using the public API. For private protocol logic, use modules in
  `tests/unit/` referenced from source with `#[cfg(test)]` and `#[path = ...]`;
  do not expose private implementation details just to relocate tests.
- Add or update tests only when they protect meaningful behavior, a realistic
  regression, an edge case, or a failure path. State the behavior at risk before
  deciding a new test is needed; existing coverage may already be sufficient.
- Prefer integration tests for behavior crossing component boundaries, especially
  persistence, transactions, CDC, synchronization, and recovery. Use focused unit
  tests when they provide clearer, cheaper coverage of nontrivial isolated logic.
- Avoid tests that mirror implementation details, assert constants or trivial
  getters, prove a mock returns what it was configured to return, or duplicate
  existing coverage without adding a distinct failure mode.
- For bug fixes, prefer a regression test that fails under the old behavior and
  passes with the fix. Reuse existing fixtures and test targets where practical.
- Do not add tests merely because a file changed or to increase the test count.

## Pre-publication compatibility

- Until publication to Cargo and npm, do not maintain schema generations,
  application protocol versions, file-format versions, or upgrade migrations.
  Recreate incompatible development fixtures explicitly; never reset data silently.
- Keep runtime source/subscription identities, revision counters, and row concurrency
  versions: these protect correctness and are unrelated to release compatibility.
- Introduce release compatibility and migrations only as part of publication work.
