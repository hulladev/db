.PHONY: check fmt fmt-check lint test bench bench-compare bench-baseline bench-check db-up db-down db-shell db-config db-test db-bench db-bench-compare cdc-test cdc-bench cdc-bench-compare sync-bench sync-bench-compare cdc-diagnostic-test sync-test server-test

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
	cargo test -p hulladb_postgres --test task_store --locked -- --ignored

db-bench:
	python3 scripts/bench.py run --suite backend

db-bench-compare:
	python3 scripts/bench.py compare --suite backend

cdc-test:
	cargo test -p hulladb_cdc --test postgres_capture --test command_sync --locked -- --ignored --test-threads=1

cdc-diagnostic-test:
	cargo test -p hulladb_cdc --features diagnostic-replica --lib --locked
	cargo test -p hulladb_cdc --features diagnostic-replica --test postgres_snapshot --locked -- --ignored --test-threads=1

cdc-bench:
	python3 scripts/bench.py run --suite cdc

cdc-bench-compare:
	python3 scripts/bench.py compare --suite cdc

sync-test:
	cargo test -p hulladb_sync --test task_feed --test command_sync --locked -- --ignored --test-threads=1

server-test:
	cargo test -p hulladb_server --test http_host --locked -- --ignored --test-threads=1

sync-bench:
	python3 scripts/bench.py run --suite sync

sync-bench-compare:
	python3 scripts/bench.py compare --suite sync

db-up:
	docker compose up --detach --wait --wait-timeout 60 postgres

db-down:
	docker compose down

db-shell:
	docker compose exec postgres psql --username db_dev --dbname db_dev
