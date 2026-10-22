-- Migration 10: environments. The services wtm starts for a worktree, the flow a worktree's
-- environment belongs to, the commands run against it, and an approval intent for the destructive
-- ones.
--
-- A service row is the record that lets a relaunch find a process again. It is keyed by
-- (worktree, service): one service of one worktree is one row, rewritten at each start and stop,
-- because what matters after a crash is the process that may still be running, not a history of
-- the ones before it. Its pid alone would not do: a pid can belong to something else by the time
-- wtm looks, so the process's start time, as the operating system reported it, is kept beside it,
-- and both have to match before wtm treats the process as its own.

CREATE TABLE env_services (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    project     TEXT    NOT NULL,
    worktree    TEXT    NOT NULL,
    service     TEXT    NOT NULL,
    state       TEXT    NOT NULL CHECK (state IN (
                    'stopped', 'starting', 'healthy', 'unhealthy', 'failed')),
    pid         INTEGER,          -- the process group wtm started; NULL for a detached service
    started     TEXT,             -- the process's start time, as `ps` reported it
    detached    INTEGER NOT NULL CHECK (detached IN (0, 1)),
    log_path    TEXT    NOT NULL,
    detail      TEXT,             -- why it is unhealthy or failed, or what answered its health check
    started_at  INTEGER,
    updated_at  INTEGER NOT NULL,
    UNIQUE (worktree, service),
    -- A pid without its start time is not an identity, and is never signalled.
    CHECK ((pid IS NULL) = (started IS NULL))
);

-- The flow a worktree's environment belongs to, and where it is. The inbox reads the project and
-- worktree from here for an environment's items, as it reads them from review_runs for a review's.
CREATE TABLE environment_flows (
    flow_id   INTEGER PRIMARY KEY REFERENCES flows (id),
    project   TEXT    NOT NULL,
    worktree  TEXT    NOT NULL UNIQUE
);

-- One run of an environment command, from the click to its exit. A destructive command on a
-- shared resource starts here as awaiting_approval, and runs only under an approval of exactly
-- what it runs: `binding` pins the hash of the rendered argv, never the argv itself, because a
-- rendered argv can hold a password from .env.
CREATE TABLE env_command_runs (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id       INTEGER NOT NULL REFERENCES flows (id),
    command       TEXT    NOT NULL,
    label         TEXT    NOT NULL,
    argv          TEXT    NOT NULL, -- as declared, templates and all, as JSON
    binding       TEXT    NOT NULL, -- the pins an approval has to cover, as JSON
    state         TEXT    NOT NULL CHECK (state IN (
                      'awaiting_approval', 'declined', 'stale', 'running', 'succeeded', 'failed')),
    approval_id   INTEGER REFERENCES approvals (id),
    exit_code     INTEGER,
    output        TEXT,             -- the end of what it printed
    requested_at  INTEGER NOT NULL,
    finished_at   INTEGER,
    CHECK ((state IN ('awaiting_approval', 'running')) = (finished_at IS NULL))
);
CREATE INDEX env_command_runs_by_flow ON env_command_runs (flow_id, id);

-- At most one command waiting for your approval per worktree at a time: its inbox item is filed
-- against the environment's flow, and the inbox files one open item per target and kind.
CREATE UNIQUE INDEX env_command_runs_one_waiting ON env_command_runs (flow_id)
    WHERE state = 'awaiting_approval';

-- How long a project's services may sit while wtm is in the background before it stops them.
-- Opt-in: no row, no idle stop.
CREATE TABLE env_idle_stop (
    project  TEXT    PRIMARY KEY,
    minutes  INTEGER NOT NULL CHECK (minutes > 0)
);

-- `command` joins the approval intents. SQLite cannot change a CHECK in place, so `approvals` is
-- rebuilt, and `operations` with it: `operations` refers to `approvals`, foreign keys are on, and
-- this runs inside the migration's transaction, where they cannot be turned off. Dropping
-- `approvals` under a referring `operations` fails even with the check deferred, because SQLite
-- counts the violation at the drop and nothing it sees afterwards uncounts it. So both new tables
-- are filled, the old ones dropped child first, and the new ones renamed into place; renaming
-- `approvals_v9` rewrites `operations_v9`'s reference along with it.

CREATE TABLE approvals_v9 (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id     INTEGER NOT NULL REFERENCES flows (id),
    intent      TEXT    NOT NULL CHECK (intent IN (
                    'plan', 'design', 'publish', 'post', 'route', 'merge', 'command')),
    binding     TEXT    NOT NULL, -- the pins it covers, as a JSON object
    subject     TEXT    NOT NULL, -- what it was of, as JSON
    granted_at  INTEGER NOT NULL
);
INSERT INTO approvals_v9 (id, flow_id, intent, binding, subject, granted_at)
SELECT id, flow_id, intent, binding, subject, granted_at FROM approvals;

CREATE TABLE operations_v9 (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id       INTEGER NOT NULL REFERENCES flows (id),
    kind          TEXT    NOT NULL,
    target        TEXT    NOT NULL, -- what it writes to, such as owner/name#40
    payload_hash  TEXT    NOT NULL,
    payload       TEXT    NOT NULL, -- exactly what it sends, as JSON; payload_hash is its hash
    approval_id   INTEGER NOT NULL REFERENCES approvals_v9 (id),
    position      INTEGER NOT NULL, -- its place in its approval's sequence
    state         TEXT    NOT NULL CHECK (state IN (
                      'pending', 'executing', 'done', 'failed', 'needs_attention', 'cancelled')),
    attempts      INTEGER NOT NULL DEFAULT 0, -- how many times it was sent
    last_error    TEXT,
    result        TEXT,             -- what it left on the remote, as JSON
    attention     TEXT,             -- why it waits for you, as JSON
    resolution    TEXT,             -- how you settled that, as JSON
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    -- The same write is one row, however many times it is approved.
    UNIQUE (flow_id, kind, target, payload_hash),
    CHECK (state != 'done' OR result IS NOT NULL),
    CHECK ((state = 'needs_attention') = (attention IS NOT NULL))
);
INSERT INTO operations_v9
    (id, flow_id, kind, target, payload_hash, payload, approval_id, position, state, attempts,
     last_error, result, attention, resolution, created_at, updated_at)
SELECT id, flow_id, kind, target, payload_hash, payload, approval_id, position, state, attempts,
       last_error, result, attention, resolution, created_at, updated_at
FROM operations;

-- Dropping a table drops its triggers and indexes; they are made again below, unchanged.
DROP TABLE operations;
DROP TABLE approvals;
ALTER TABLE approvals_v9 RENAME TO approvals;
ALTER TABLE operations_v9 RENAME TO operations;

CREATE INDEX approvals_by_flow ON approvals (flow_id);

CREATE TRIGGER approvals_are_never_updated BEFORE UPDATE ON approvals
BEGIN
    SELECT RAISE(ABORT, 'approvals is append-only');
END;

CREATE TRIGGER approvals_are_never_deleted BEFORE DELETE ON approvals
BEGIN
    SELECT RAISE(ABORT, 'approvals is append-only');
END;

CREATE INDEX operations_by_approval ON operations (approval_id, position);
CREATE INDEX operations_executing ON operations (state) WHERE state = 'executing';

-- A write recorded as done happened, and nothing may queue it again: the store's rules refuse it
-- first, and this refuses it for anything that reaches the table some other way.
CREATE TRIGGER operations_stay_done BEFORE UPDATE OF state ON operations
WHEN OLD.state = 'done' AND NEW.state != 'done'
BEGIN
    SELECT RAISE(ABORT, 'a done operation stays done');
END;

CREATE TRIGGER operations_are_never_deleted BEFORE DELETE ON operations
BEGIN
    SELECT RAISE(ABORT, 'operations are never deleted');
END;
