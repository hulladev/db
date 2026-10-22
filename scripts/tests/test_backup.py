import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from urllib.parse import urlsplit, unquote
import uuid

SCRIPT = Path(__file__).resolve().parents[1] / "backup.py"

@unittest.skipUnless(os.environ.get("HULLADB_OPERATIONS_TEST_URL"), "requires explicit PostgreSQL operations test URL")
class BackupRestoreTest(unittest.TestCase):
    def test_restore_preserves_rows_receipts_and_outbox_rotates_source_and_refuses_overwrite(self):
        url = urlsplit(os.environ["HULLADB_OPERATIONS_TEST_URL"])
        env = {**os.environ, "PGHOST": url.hostname, "PGPORT": str(url.port or 5432), "PGUSER": unquote(url.username or ""), "PGPASSWORD": unquote(url.password or ""), "PGDATABASE": url.path.lstrip("/")}
        namespace = "backup_" + uuid.uuid4().hex
        database = "restore_" + uuid.uuid4().hex
        original_source, scope, request = map(str, [uuid.uuid4(), uuid.uuid4(), uuid.uuid4()])
        job = str(uuid.uuid4())
        expired_request = str(uuid.uuid4())
        def sql(query, target=env):
            result = subprocess.run(["psql", "-X", "-At", "--no-password", "--set", "ON_ERROR_STOP=1"], input=query, text=True, capture_output=True, env=target)
            self.assertEqual(result.returncode, 0, "PostgreSQL fixture command failed")
            return result.stdout.strip()
        target = {**env, "PGDATABASE": database}
        sql(f'CREATE SCHEMA "{namespace}"; CREATE TABLE "{namespace}".metadata(singleton boolean,source uuid,contract text); INSERT INTO "{namespace}".metadata VALUES(true,\'{original_source}\',\'{{}}\'); CREATE TABLE "{namespace}".scopes(id uuid,sequence bigint); INSERT INTO "{namespace}".scopes VALUES(\'{scope}\',42); CREATE TABLE "{namespace}".t0(id uuid,text text); INSERT INTO "{namespace}".t0 VALUES(\'{request}\',\'restored data\'); CREATE TABLE "{namespace}".receipts(request_id uuid,receipt jsonb,created_at timestamptz NOT NULL DEFAULT clock_timestamp(),expired boolean NOT NULL DEFAULT false); INSERT INTO "{namespace}".receipts(request_id,receipt) VALUES(\'{request}\',\'{{"source":"{original_source}","result":"kept"}}\');')
        sql(f"INSERT INTO \"{namespace}\".receipts(request_id,receipt,expired) VALUES('{expired_request}','{{\"source\":\"{original_source}\",\"result\":null}}',true)")
        created_at = sql(f'SELECT created_at FROM "{namespace}".receipts WHERE expired')
        sql(f'CREATE TABLE "{namespace}".outbox(id uuid,source uuid,scope uuid,payload jsonb); INSERT INTO "{namespace}".outbox VALUES(\'{job}\',\'{original_source}\',\'{scope}\',\'{{"action":"retained"}}\');')
        sql(f'CREATE DATABASE "{database}"')
        try:
            with tempfile.TemporaryDirectory() as temporary:
                archive = Path(temporary) / "backup.dump"
                saved = subprocess.run([sys.executable, str(SCRIPT), "backup", "--schema", namespace, "--output", str(archive)], env=env, capture_output=True, text=True)
                self.assertEqual(saved.returncode, 0, saved.stderr)
                manifest = json.loads(Path(str(archive) + ".json").read_text())
                self.assertEqual(manifest["source"], original_source)
                self.assertEqual(archive.stat().st_mode & 0o777, 0o600)
                duplicate = subprocess.run([sys.executable, str(SCRIPT), "backup", "--schema", namespace, "--output", str(archive)], env=env, capture_output=True, text=True)
                self.assertNotEqual(duplicate.returncode, 0)
                manifest_path = Path(str(archive) + ".json")
                manifest_path.write_text(json.dumps({**manifest, "source": str(uuid.uuid4())}))
                mismatched = subprocess.run([sys.executable, str(SCRIPT), "restore", "--archive", str(archive)], env=target, capture_output=True, text=True)
                self.assertNotEqual(mismatched.returncode, 0)
                self.assertEqual(sql(f"SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname='{namespace}')", target), "f")
                manifest_path.write_text(json.dumps(manifest))
                restored = subprocess.run([sys.executable, str(SCRIPT), "restore", "--archive", str(archive)], env=target, capture_output=True, text=True)
                self.assertEqual(restored.returncode, 0, restored.stderr)
                self.assertNotEqual(sql(f'SELECT source FROM "{namespace}".metadata', target), original_source)
                self.assertEqual(sql(f'SELECT sequence FROM "{namespace}".scopes', target), "42")
                self.assertEqual(sql(f'SELECT text FROM "{namespace}".t0', target), "restored data")
                self.assertEqual(sql(f"SELECT receipt->>'source' FROM \"{namespace}\".receipts WHERE NOT expired", target), original_source)
                self.assertEqual(sql(f'SELECT request_id FROM "{namespace}".receipts WHERE expired', target), expired_request)
                self.assertEqual(sql(f'SELECT created_at FROM "{namespace}".receipts WHERE expired', target), created_at)
                self.assertEqual(sql(f"SELECT receipt->'result' FROM \"{namespace}\".receipts WHERE expired", target), "null")
                self.assertEqual(sql(f'SELECT id FROM "{namespace}".outbox', target), job)
                self.assertEqual(sql(f'SELECT source FROM "{namespace}".outbox', target), original_source)
                self.assertEqual(sql(f"SELECT payload->>'action' FROM \"{namespace}\".outbox", target), "retained")
                again = subprocess.run([sys.executable, str(SCRIPT), "restore", "--archive", str(archive)], env=target, capture_output=True, text=True)
                self.assertNotEqual(again.returncode, 0)
                self.assertIn("already exists", again.stderr)
                with archive.open("ab") as output:
                    output.write(b"corruption")
                rejected = subprocess.run([sys.executable, str(SCRIPT), "restore", "--archive", str(archive)], env=target, capture_output=True, text=True)
                self.assertNotEqual(rejected.returncode, 0)
                self.assertIn("checksum", rejected.stderr)
                self.assertEqual(sql(f'SELECT count(*) FROM "{namespace}".t0', target), "1")
        finally:
            sql(f'DROP DATABASE "{database}" WITH (FORCE)')
            sql(f'DROP SCHEMA "{namespace}" CASCADE')
