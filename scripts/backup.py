#!/usr/bin/env python3
"""Schema-scoped backups and non-destructive restores. Uses libpq PG* environment.

Archive/manifest files are operator-owned and must be trusted: PostgreSQL restore
can execute SQL from an archive. Restore requires an absent target namespace and
rotates source before making restored data available. No roles/passwords are saved.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
from datetime import datetime, timezone


def identifier(value):
    if not re.fullmatch(r"[A-Za-z0-9_]{1,63}", value) or value.startswith("pg_") or value == "public":
        raise ValueError("use a dedicated ASCII PostgreSQL namespace")
    return '"' + value + '"'


def run(args, *, sql=None):
    environment = dict(os.environ)
    environment.setdefault("PGCONNECT_TIMEOUT", "5")
    if environment.get("HULLADB_PG_PASSWORD_FILE"):
        environment["PGPASSWORD"] = Path(environment["HULLADB_PG_PASSWORD_FILE"]).read_text().strip()
    result = subprocess.run(args, input=sql, text=True, capture_output=True, check=False, env=environment)
    if result.returncode:
        # Never echo credentials, SQL bodies or COPY data; keep the first diagnostic.
        diagnostic = result.stderr.strip().splitlines()[0] if result.stderr.strip() else "no diagnostic"
        raise RuntimeError(f"{Path(args[0]).name} failed with exit status {result.returncode}: {diagnostic}")
    return result.stdout.strip()


def query(sql):
    return run(["psql", "-X", "--no-password", "--set", "ON_ERROR_STOP=1", "--tuples-only", "--no-align"], sql=sql)


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def source(namespace):
    return query(f"SELECT source FROM {identifier(namespace)}.metadata WHERE singleton;")


def backup(namespace, output):
    name = identifier(namespace)
    output = Path(output).resolve()
    manifest = output.with_name(output.name + ".json")
    if output.exists() or manifest.exists():
        raise ValueError("backup output or manifest already exists; choose a new path")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="hulladb-backup-", dir=output.parent) as temporary:
        archive = Path(temporary) / "archive.dump"
        # The quoted pattern matches one exact namespace, including case.
        run(["pg_dump", "--no-password", "--format=custom", "--no-owner", "--no-acl", "--lock-wait-timeout=5s", f"--schema={name}", f"--file={archive}"])
        # Read the source from the dump's consistent snapshot, not a later live
        # query which could race an operator source rotation. pg_restore prints
        # the metadata COPY section from this trusted schema-only archive.
        metadata = run(["pg_restore", "--data-only", "--table=metadata", "--file=-", str(archive)])
        rows = []
        copying = False
        for line in metadata.splitlines():
            if line.startswith("COPY ") and ".metadata " in line:
                copying = True
            elif copying and line == r"\.":
                copying = False
            elif copying:
                rows.append(line.split("\t"))
        if len(rows) != 1 or len(rows[0]) != 3 or rows[0][0] != "t":
            raise ValueError("archive does not contain exactly one managed source")
        import uuid
        deployment_source = str(uuid.UUID(rows[0][1]))
        metadata_path = Path(temporary) / "manifest.json"
        metadata_path.write_text(json.dumps({"schema": namespace, "source": deployment_source, "sha256": digest(archive), "created_utc": datetime.now(timezone.utc).isoformat()}, indent=2) + "\n")
        os.chmod(archive, 0o600)
        os.chmod(metadata_path, 0o600)
        for path in (archive, metadata_path):
            with path.open("rb") as durable:
                os.fsync(durable.fileno())
        # Hard links publish without overwriting a concurrently created output.
        os.link(archive, output)
        try:
            os.link(metadata_path, manifest)
        except BaseException:
            output.unlink()
            raise
    return manifest


def restore(archive):
    archive = Path(archive).resolve()
    manifest = json.loads(archive.with_name(archive.name + ".json").read_text())
    if set(manifest) != {"schema", "source", "sha256", "created_utc"}:
        raise ValueError("invalid backup manifest")
    name = identifier(manifest["schema"])
    import uuid
    original = str(uuid.UUID(manifest["source"]))
    if digest(archive) != manifest["sha256"]:
        raise ValueError("backup checksum does not match")
    namespace = manifest["schema"]
    if query(f"SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname='{namespace}');") != "f":
        raise ValueError("target namespace already exists; restore into a separate database")
    # Render trusted archive SQL into a private file, then append rotation inside
    # the same psql transaction. A failure in either restore or rotation leaves
    # no partially restored namespace and never publishes the old source.
    with tempfile.TemporaryDirectory(prefix="hulladb-restore-") as temporary:
        sql = Path(temporary) / "restore.sql"
        run(["pg_restore", "--no-owner", "--no-acl", f"--file={sql}", str(archive)])
        with sql.open("a") as target:
            target.write(f"\nDO $$ BEGIN IF (SELECT source FROM {name}.metadata WHERE singleton) IS DISTINCT FROM '{original}'::uuid THEN RAISE EXCEPTION 'backup source mismatch'; END IF; END $$;\n")
            target.write(f"UPDATE {name}.metadata SET source=gen_random_uuid() WHERE singleton;\n")
        run(["psql", "-X", "--no-password", "--set", "ON_ERROR_STOP=1", "--single-transaction", f"--file={sql}"])
    return source(namespace)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    save = commands.add_parser("backup")
    save.add_argument("--schema", required=True)
    save.add_argument("--output", required=True)
    load = commands.add_parser("restore")
    load.add_argument("--archive", required=True)
    args = parser.parse_args()
    try:
        if args.command == "backup":
            print(f"Backup saved: {backup(args.schema, args.output)}")
        else:
            print(f"Restore complete; new source: {restore(args.archive)}")
    except (OSError, ValueError, RuntimeError, KeyError, TypeError) as error:
        parser.exit(1, f"Operation failed: {error}\n")


if __name__ == "__main__":
    main()
