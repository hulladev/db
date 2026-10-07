CREATE SCHEMA IF NOT EXISTS _hulladb_task_probe;
-- Persisted source identity detects replacements/restores; it is not a schema version.
CREATE TABLE IF NOT EXISTS _hulladb_task_probe.metadata (
    singleton boolean PRIMARY KEY CHECK (singleton),
    source uuid NOT NULL DEFAULT gen_random_uuid()
);
INSERT INTO _hulladb_task_probe.metadata(singleton) VALUES (true) ON CONFLICT DO NOTHING;
CREATE TABLE IF NOT EXISTS _hulladb_task_probe.workspaces (
    id uuid PRIMARY KEY, sequence bigint NOT NULL DEFAULT 0 CHECK (sequence >= 0)
);
CREATE TABLE IF NOT EXISTS _hulladb_task_probe.members (
    workspace uuid NOT NULL REFERENCES _hulladb_task_probe.workspaces(id),
    principal uuid NOT NULL, can_write boolean NOT NULL,
    PRIMARY KEY (workspace, principal)
);
CREATE TABLE IF NOT EXISTS _hulladb_task_probe.tasks (
    workspace uuid NOT NULL REFERENCES _hulladb_task_probe.workspaces(id),
    id uuid NOT NULL, title text NOT NULL CHECK (octet_length(title) BETWEEN 1 AND 4096),
    completed boolean NOT NULL, version bigint NOT NULL CHECK (version > 0),
    PRIMARY KEY (workspace, id)
);
CREATE INDEX IF NOT EXISTS tasks_by_status ON _hulladb_task_probe.tasks(workspace, completed, id);
CREATE TABLE IF NOT EXISTS _hulladb_task_probe.receipts (
    workspace uuid NOT NULL REFERENCES _hulladb_task_probe.workspaces(id),
    principal uuid NOT NULL, request_id uuid NOT NULL,
    payload jsonb NOT NULL, receipt jsonb NOT NULL,
    PRIMARY KEY (workspace, principal, request_id)
);
