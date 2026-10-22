-- Migration 11: Start Work. The job that gives a started issue its worktree, and an approval
-- intent for moving an issue to another team status.
--
-- A start job is keyed by its work item, not numbered: "is this issue being started already?" is
-- the question every Start asks first, and a primary key is how the database answers it for two
-- clicks at once. Starting the issue again after its run ended rewrites the row for the new run;
-- the worktrees the item used stay in work_item_links, which is the item's history.
--
-- It is written before `git worktree add` runs, with the place being made, so a relaunch after a
-- crash knows what was under way and whether it got as far as the worktree.

CREATE TABLE start_jobs (
    work_item_id   INTEGER PRIMARY KEY REFERENCES work_items (id),
    flow_id        INTEGER NOT NULL UNIQUE REFERENCES flows (id),
    project        TEXT    NOT NULL,
    issue_key      TEXT    NOT NULL,
    candidate      TEXT    NOT NULL, -- what resolution offered and was picked, as JSON
    worktree       TEXT    NOT NULL, -- its path: where it is, or where it is being made
    branch         TEXT,
    state          TEXT    NOT NULL CHECK (state IN (
                       'creating', 'setting_up', 'ready', 'setup_failed', 'failed', 'interrupted')),
    detail         TEXT,             -- why it failed, or what its setup said when it did
    setup_session  TEXT,             -- the terminal its setup runs in, while a process has it
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);
CREATE INDEX start_jobs_by_project ON start_jobs (project);

-- `move` joins the approval intents. SQLite cannot change a CHECK in place, so `approvals` is
-- rebuilt, and with it the two tables that refer to it, `operations` and `env_command_runs`:
-- foreign keys are on, and this runs inside the migration's transaction, where they cannot be
-- turned off. Dropping `approvals` under a referring row fails even with the check deferred,
-- because SQLite counts the violation at the drop and nothing it sees afterwards uncounts it, and
-- an environment command that was approved is such a row. So the three new tables are filled, the
-- old ones dropped children first, and the new ones renamed into place; renaming `approvals_v11`
-- rewrites both children's references along with it. Migration 10's intents are all kept.

CREATE TABLE approvals_v11 (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id     INTEGER NOT NULL REFERENCES flows (id),
    intent      TEXT    NOT NULL CHECK (intent IN (
                    'plan', 'design', 'publish', 'post', 'route', 'merge', 'command', 'move')),
    binding     TEXT    NOT NULL, -- the pins it covers, as a JSON object
    subject     TEXT    NOT NULL, -- what it was of, as JSON
    granted_at  INTEGER NOT NULL
);
INSERT INTO approvals_v11 (id, flow_id, intent, binding, subject, granted_at)
SELECT id, flow_id, intent, binding, subject, granted_at FROM approvals;

CREATE TABLE operations_v11 (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id       INTEGER NOT NULL REFERENCES flows (id),
    kind          TEXT    NOT NULL,
    target        TEXT    NOT NULL, -- what it writes to, such as owner/name#40 or jira:KEY-1
    payload_hash  TEXT    NOT NULL,
    payload       TEXT    NOT NULL, -- exactly what it sends, as JSON; payload_hash is its hash
    approval_id   INTEGER NOT NULL REFERENCES approvals_v11 (id),
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
INSERT INTO operations_v11
    (id, flow_id, kind, target, payload_hash, payload, approval_id, position, state, attempts,
     last_error, result, attention, resolution, created_at, updated_at)
SELECT id, flow_id, kind, target, payload_hash, payload, approval_id, position, state, attempts,
       last_error, result, attention, resolution, created_at, updated_at
FROM operations;

-- Migration 10's table, unchanged but for the table its approval refers to.
CREATE TABLE env_command_runs_v11 (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id       INTEGER NOT NULL REFERENCES flows (id),
    command       TEXT    NOT NULL,
    label         TEXT    NOT NULL,
    argv          TEXT    NOT NULL, -- as declared, templates and all, as JSON
    binding       TEXT    NOT NULL, -- the pins an approval has to cover, as JSON
    state         TEXT    NOT NULL CHECK (state IN (
                      'awaiting_approval', 'declined', 'stale', 'running', 'succeeded', 'failed')),
    approval_id   INTEGER REFERENCES approvals_v11 (id),
    exit_code     INTEGER,
    output        TEXT,             -- the end of what it printed
    requested_at  INTEGER NOT NULL,
    finished_at   INTEGER,
    CHECK ((state IN ('awaiting_approval', 'running')) = (finished_at IS NULL))
);
INSERT INTO env_command_runs_v11
    (id, flow_id, command, label, argv, binding, state, approval_id, exit_code, output,
     requested_at, finished_at)
SELECT id, flow_id, command, label, argv, binding, state, approval_id, exit_code, output,
       requested_at, finished_at
FROM env_command_runs;

-- Dropping a table drops its triggers and indexes; they are made again below, unchanged.
DROP TABLE env_command_runs;
DROP TABLE operations;
DROP TABLE approvals;
ALTER TABLE approvals_v11 RENAME TO approvals;
ALTER TABLE operations_v11 RENAME TO operations;
ALTER TABLE env_command_runs_v11 RENAME TO env_command_runs;

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

CREATE INDEX env_command_runs_by_flow ON env_command_runs (flow_id, id);

-- At most one command waiting for your approval per worktree at a time: its inbox item is filed
-- against the environment's flow, and the inbox files one open item per target and kind.
CREATE UNIQUE INDEX env_command_runs_one_waiting ON env_command_runs (flow_id)
    WHERE state = 'awaiting_approval';
