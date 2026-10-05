CREATE SCHEMA IF NOT EXISTS _hulladb_task_probe;
CREATE TABLE IF NOT EXISTS _hulladb_task_probe.metadata (
    singleton boolean PRIMARY KEY CHECK (singleton), generation integer NOT NULL
);
INSERT INTO _hulladb_task_probe.metadata VALUES (true, 2) ON CONFLICT DO NOTHING;
DO $$ BEGIN
    IF (SELECT generation FROM _hulladb_task_probe.metadata) NOT IN (1, 2) THEN
        RAISE EXCEPTION 'unsupported task probe schema generation';
    END IF;
END $$;
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
-- Upgrade the earlier internal schema atomically. Receipts and authoritative
-- rows survive; the command-written feed has no role in CDC synchronization.
DO $$ BEGIN
    IF (SELECT generation FROM _hulladb_task_probe.metadata) = 1 THEN
        IF EXISTS (SELECT 1 FROM _hulladb_task_probe.receipts
                   WHERE jsonb_array_length(receipt->'batch'->'changes') IS DISTINCT FROM 1) THEN
            RAISE EXCEPTION 'cannot migrate malformed command receipts';
        END IF;
        UPDATE _hulladb_task_probe.receipts SET receipt = jsonb_build_object(
            'request_id', receipt->'request_id',
            'revision', (receipt->'batch'->'cursor') || '{"generation":2}'::jsonb,
            'change', receipt->'batch'->'changes'->0);
        DROP TABLE IF EXISTS _hulladb_task_probe.changes;
        UPDATE _hulladb_task_probe.metadata SET generation=2;
    END IF;
END $$;
