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
  `cargo test -p hulladb_postgres --test managed_functions --locked -- --ignored`, or
  `cargo test -p hulladb_cdc --test <target> --locked -- --ignored --test-threads=1`
  where the CDC target is `postgres_capture` or `postgres_snapshot` (with `--features diagnostic-replica`).
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
- Before retaining a new test, identify a plausible production bug it would
  catch and the observable consequence. A function being callable or returning
  its configured value is not enough. Explain this in the change summary; add a
  test comment only when the failure mode is not clear from the test itself.
- Expected results must be independent of the implementation under test. Do not
  regenerate a fixture with that implementation and then compare its output to
  the freshly generated fixture. Generated fixtures are useful when a separate
  consumer compiles, validates, or executes them against explicit expectations.
- Test doubles should control inputs, timing, and failures at a real production
  boundary. Do not build transaction, authorization, or subscription behavior
  into a fixture and count assertions about that behavior as product coverage.
- Give each failure mode a primary test at the cheapest effective layer. Keep
  additional integration or end-to-end coverage only when it protects distinct
  wiring, runtime, or persistence behavior. Reuse an existing scenario for
  related assertions instead of repeating expensive generation or startup.
- Temporary development checks may help verify an implementation, but remove
  them before handoff unless they meet these regression criteria. Do not retain
  scratch tests, generated-output string checks, or tests of language/library
  guarantees when behavioral or compile-time coverage already protects the risk.
- For cleanup, name the removed test's redundant coverage or explain why it
  cannot catch a meaningful product regression. Never use a deletion quota,
  test count, or runtime alone to justify dropping distinct coverage.

## Process ownership and cleanup

- Track task-started long-running commands by tool session, PID/process group,
  purpose and owning agent. The orchestrator owns final cleanup even when a
  subagent started the command. Do not start a replacement compiler/test command
  until the previous instance has exited, unless parallel runs are intentional.
- A timeout, cancellation request or successful signal delivery is not proof of
  process exit. Wait for completion and verify that owned children are gone;
  killing a shell or launcher alone can orphan its compiler descendants.
- The repo's Codex hook tags shell commands and cleans up tagged processes on
  Stop, Interrupt and SessionEnd, including children started by subagents. Keep
  the ownership environment marker intact. This is a cleanup hook, not a process
  supervisor; do not add a daemon or supervisor for routine agent commands.
- Check for owned leftovers when a subagent finishes and before final handoff.
  Revalidate PID ownership/start time before signaling a process discovered later.
  Never use broad name-based cleanup such as `pkill tsc` or terminate another
  agent/client's processes merely because they use this checkout.
- Keep background services only while the task requires them; the hook cleans
  tagged services at turn boundaries too. Services meant to persist should be
  started from the user's terminal. If process inspection is blocked, report
  cleanup as unverified rather than claiming termination succeeded.

## Implementation footprint

- Prefer Bun-native APIs in Bun-only TypeScript code generation, development tooling
  and build scripts: `Bun.file`/`Bun.write` for file contents, `Bun.spawn`/`spawnSync`
  and native IPC for subprocesses, `Bun.build` for bundling, and Bun utilities where
  they fit. Changing the executable alone is not an API migration. Use Node-compatible
  APIs where Bun recommends them or lacks a suitable equivalent, including path
  operations, directory watching, directory creation and symlink-aware inspection.
- Keep shared production TypeScript runtime code and generated application code
  compatible with both Node and Bun; browser code must remain browser-compatible.
  Do not introduce unconditional Bun globals into these surfaces. Choose APIs by
  execution environment and semantics, not by import spelling; avoid duplicate
  implementations or claims of faster performance without measurements. Validate
  runtime changes on both Node and Bun when they affect compatibility.
- Remove superseded execution paths, exports, dependencies, fixtures and tooling
  when their replacement is in use. Do not ship a test-only implementation or
  retain an old path behind a flag solely as an archive; Git history serves that
  purpose. Trace callers, generated imports, feature combinations and diagnostic
  consumers before deleting code. Absence of a local caller alone does not make a
  supported public API dead.
- Keep mocks and fault-injection helpers under tests, with no fallback from
  production execution into simulated behavior. Clean up temporary proof code and
  generated scratch projects before handoff.

## Pre-publication compatibility

- Until publication to Cargo and npm, do not maintain schema generations,
  application protocol versions, file-format versions, or upgrade migrations.
  Recreate incompatible development fixtures explicitly; never reset data silently.
- Keep runtime source/subscription identities, revision counters, and row concurrency
  versions: these protect correctness and are unrelated to release compatibility.
- Introduce release compatibility and migrations only as part of publication work.

## Dependency footprint

- Separate production dependencies from build, generation and test tooling. Keep
  optional capabilities behind features; do not rely on dev-dependency feature
  unification to make production builds compile.
- Before adding a dependency or enabling default features, identify the required
  behavior and check whether existing code or a small standard-library helper
  suffices. Keep maintained implementations for cryptography, TLS, parsing and
  async runtimes rather than replacing them with bespoke security-critical code.
- Inspect the dependency graph of the shipped entrypoint, not just the workspace
  lockfile. Check advisories against resolved versions when dependencies change;
  distinguish reachable runtime features from tooling and target-only dependencies.
- Keep disposable caches, queued work and subprocess lifetimes bounded. Preserve
  durable recovery evidence until explicit application acknowledgement or removal.
