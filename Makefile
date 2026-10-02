.PHONY: check fmt fmt-check lint test db-up db-down db-shell db-config

check: fmt-check lint test

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets --locked -- -D warnings

test:
	cargo test --workspace --locked

db-config:
	docker compose config --quiet

db-up:
	docker compose up --detach --wait --wait-timeout 60 postgres

db-down:
	docker compose down

db-shell:
	docker compose exec postgres psql --username db_dev --dbname db_dev
