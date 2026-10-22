.PHONY: check fmt fmt-check lint test bench bench-compare bench-baseline bench-check db-up db-down db-shell db-config db-test cdc-test cdc-bench cdc-bench-compare cdc-diagnostic-test server-test client-test sdk-check

BENCH_BASELINE ?= benchmarks/baselines/local.json

check: fmt-check lint test bench-check

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets --locked -- -D warnings

test:
	cargo test --workspace --locked

bench:
	python3 scripts/bench.py run

bench-compare:
	python3 scripts/bench.py compare --baseline "$(BENCH_BASELINE)"

bench-baseline:
	python3 scripts/bench.py save --output "$(BENCH_BASELINE)"

bench-check:
	python3 -m unittest discover -s scripts/tests -v

db-config:
	docker compose config --quiet

db-test:
	cargo test -p hulladb_postgres --test managed_functions --locked -- --ignored

.PHONY: rust-authoring-test
rust-authoring-test:
	cargo test -p hulladb --locked
	cargo test -p hulladb --test functions --locked -- --ignored

.PHONY: sdk-adapters-test
sdk-adapters-test: sdk-check
	cargo test -p hulladb_server --test managed_host --locked -- --ignored

.PHONY: typescript-functions-test
typescript-functions-test: sdk-check
	cargo build -p hulladb_js --locked
	cargo test -p hulladb_postgres --features typescript --test typescript_functions --test typescript_contracts --locked -- --include-ignored
	cargo test -p hulladb_server --features typescript --test managed_host --locked generated_sdk_executes_typescript_callbacks -- --ignored

.PHONY: better-auth-test
better-auth-test:
	cargo test -p hulladb_server --features typescript --test direct_bridge --locked better_auth_storage -- --ignored --test-threads=1

cdc-test:
	cargo test -p hulladb_cdc --test postgres_capture --locked -- --ignored --test-threads=1

cdc-diagnostic-test:
	cargo test -p hulladb_cdc --features diagnostic-replica --lib --locked
	cargo test -p hulladb_cdc --features diagnostic-replica --test postgres_snapshot --locked -- --ignored --test-threads=1

cdc-bench:
	python3 scripts/bench.py run --suite cdc

cdc-bench-compare:
	python3 scripts/bench.py compare --suite cdc

server-test:
	cargo test -p hulladb_server --test managed_host --locked -- --ignored --test-threads=1

sdk-check:
	cd packages/db && bun run check

client-test:
	cargo test -p hulladb_client --features http --locked

db-up:
	docker compose up --detach --wait --wait-timeout 60 postgres

db-down:
	docker compose down

db-shell:
	docker compose exec postgres psql --username db_dev --dbname db_dev

.PHONY: production-check production-test
production-check:
	cargo fmt -p hulladb -p hulladb_postgres -p hulladb_server -- --check
	cargo clippy -p hulladb -p hulladb_postgres -p hulladb_server --all-targets --locked -- -D warnings
	cargo test -p hulladb_server --lib --locked
	cargo test -p hulladb_postgres --test production_operations --locked
	cd packages/db && bun run check

production-test:
	cargo test -p hulladb_postgres --test production_operations --locked -- --ignored
	cargo test -p hulladb --test admin --locked -- --ignored
	cargo test -p hulladb_server --test managed_host --locked host_admission -- --ignored
	HULLADB_OPERATIONS_TEST_URL="$${HULLADB_OPERATIONS_TEST_URL:?Set a privileged PostgreSQL operations test URL}" python3 -m unittest scripts.tests.test_backup -v

.PHONY: remote-functions-check remote-functions-test

.PHONY: outbox-test
outbox-test:
	cargo test -p hulladb_postgres --test outbox --locked -- --include-ignored
	cargo test -p hulladb --test admin --locked -- --ignored

remote-functions-check:
	cargo check -p hulladb --no-default-features --features remote --example remote --locked
	cargo test -p hulladb --no-default-features --features remote --test authoring --test remote --locked
	cargo test -p hulladb_client --features http --lib --locked

remote-functions-test: remote-functions-check
	cargo test -p hulladb --features remote --test remote --locked -- --ignored

.PHONY: operations-visibility-test
operations-visibility-test:
	cargo test -p hulladb_postgres --test production_operations --locked -- --include-ignored
	cargo test -p hulladb_server --test managed_host --locked host_ -- --ignored
	cargo test -p hulladb_server --test managed_host --locked shared_subscriptions -- --ignored
	cargo test -p hulladb --no-default-features --features remote --test remote --locked expired_recovery
	cd packages/db && bun run check

.PHONY: direct-server-test
direct-server-test:
	cargo test -p hulladb_server --features typescript --test direct_bridge --locked -- --ignored

.PHONY: browser-service-test
browser-service-test:
	cargo build -p hulladb_js --locked
	cargo test -p hulladb_server --features typescript --test service_protocol --locked
	cargo test -p hulladb_server --features typescript --test service --locked -- --ignored
