#!/bin/sh
set -eu
# Executed by the official PostgreSQL image only for an empty data directory.
# Passwords travel via stdin, never process arguments or environment output.
# SQL strings are quoted defensively even though setup emits URL-safe secrets.
quote_literal() { sed "s/'/''/g" "$1" | tr -d '\n'; }
{
  printf 'CREATE ROLE hulladb_runtime LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD '\''%s'\'';\n' "$(quote_literal /run/secrets/runtime_password)"
  printf 'CREATE ROLE hulladb_backup LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD '\''%s'\'';\n' "$(quote_literal /run/secrets/backup_password)"
} | psql --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" --set ON_ERROR_STOP=1
