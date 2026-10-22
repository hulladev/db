#!/usr/bin/env python3
"""Generate private, local Compose secrets once. Never prints secret values."""
import json
import os
from pathlib import Path
import secrets
from urllib.parse import quote
import uuid


def setup(destination):
    destination = Path(destination)
    destination.mkdir(mode=0o700)  # Existing directories are deliberately refused.
    password, runtime_password, backup_password, bearer = [secrets.token_urlsafe(36) for _ in range(4)]
    credential = {"bearer": bearer, "scope": str(uuid.uuid4()), "principal": str(uuid.uuid4()), "authorization_scope": str(uuid.uuid4()), "identity": "operator-created-user"}
    values = {
        "postgres_password": password,
        "runtime_password": runtime_password,
        "backup_password": backup_password,
        "admin_url": f"postgresql://postgres:{quote(password, safe='')}@postgres:5432/hulladb?sslmode=disable",
        "runtime_url": f"postgresql://hulladb_runtime:{quote(runtime_password, safe='')}@postgres:5432/hulladb?sslmode=disable",
        "credentials.json": json.dumps([credential], indent=2),
    }
    for name, value in values.items():
        path = destination / name
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o400)
        with os.fdopen(descriptor, "w") as output:
            output.write(value + "\n")
        # File-backed Compose secrets retain host modes. The parent is private
        # (0700); mounted files must be readable by the unprivileged container UID.
        path.chmod(0o444)
    return credential["scope"]


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", default=str(Path(__file__).parent / "secrets"))
    args = parser.parse_args()
    try:
        setup(args.directory)
        print("Private secrets generated; inspect credentials.json locally to configure a client")
    except OSError as error:
        parser.exit(1, f"Setup failed: {error}\n")
