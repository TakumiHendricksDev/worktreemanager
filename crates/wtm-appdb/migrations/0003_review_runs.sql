-- Migration 3: what a review run is of, its snapshot, its steps and their attempts, and its inbox.
--
-- A review run is a flow (migration 1) plus these rows. They are written while the run works and
-- read back by the Work view and by recovery at launch, which is why a step's status and an
-- invocation's session are columns rather than something the engine only holds in memory.

-- What a review flow is of. One row per review flow, written when it starts.
CREATE TABLE review_runs (
    flow_id        INTEGER PRIMARY KEY REFERENCES flows (id),
    project        TEXT    NOT NULL,
    worktree       TEXT    NOT NULL, -- the worktree's id
    worktree_path  TEXT    NOT NULL, -- where it was, kept for the record after it is gone
    recipe         TEXT    NOT NULL,
    base           TEXT    NOT NULL, -- the ref the change is measured from, as asked for
    snapshot_id    INTEGER REFERENCES snapshots (id),
    created_at     INTEGER NOT NULL
);
CREATE INDEX review_runs_by_worktree ON review_runs (project, worktree);

-- What an invocation read. The tree object is collectable by `git gc`; the patch file under the
-- flow's directory, whose hash is here, is the durable evidence.
CREATE TABLE snapshots (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id       INTEGER NOT NULL REFERENCES flows (id),
    head_sha      TEXT    NOT NULL,
    tree_sha      TEXT    NOT NULL,
    base_sha      TEXT    NOT NULL,
    patch_path    TEXT    NOT NULL, -- relative to the flow's directory
    patch_sha256  TEXT    NOT NULL,
    patch_bytes   INTEGER NOT NULL,
    taken_at      INTEGER NOT NULL
);

-- One role of one run.
CREATE TABLE steps (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id         INTEGER NOT NULL REFERENCES flows (id),
    stage           INTEGER NOT NULL,
    role            TEXT    NOT NULL,
    provider        TEXT    NOT NULL,
    model           TEXT,
    profile         TEXT    NOT NULL,
    workspace       TEXT    NOT NULL,
    timeout_ms      INTEGER NOT NULL,
    retries         INTEGER NOT NULL,
    status          TEXT    NOT NULL CHECK (status IN (
                        'pending', 'running', 'waiting_capacity', 'done', 'failed', 'replaced',
                        'interrupted', 'cancelled', 'skipped')),
    failures        INTEGER NOT NULL DEFAULT 0,
    stands_in_for   TEXT,    -- the role a fallback runs in place of
    handover        TEXT,    -- what that role got to, as text
    detail          TEXT,
    resets_at       INTEGER, -- Unix seconds, when a provider said
    result          TEXT,    -- the Submission, as JSON, once there is one
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    -- A role's result, its retries and its fallback are all found by its id within the run.
    UNIQUE (flow_id, role),
    -- Done means a result passed its schema. The other way round is not required: a step run again
    -- keeps its last result until the new one replaces it.
    CHECK (status != 'done' OR result IS NOT NULL)
);

-- One attempt at a step: one session.
CREATE TABLE invocations (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id           INTEGER NOT NULL REFERENCES flows (id),
    step_id           INTEGER NOT NULL REFERENCES steps (id),
    attempt           INTEGER NOT NULL,
    role              TEXT    NOT NULL,
    provider          TEXT    NOT NULL,
    model             TEXT,
    profile           TEXT    NOT NULL,
    snapshot_id       INTEGER REFERENCES snapshots (id),
    cwd               TEXT    NOT NULL,
    session           TEXT,    -- wtm's session id, once it opened
    provider_session  TEXT,    -- the CLI's own conversation id
    status            TEXT    NOT NULL CHECK (status IN (
                          'opening', 'running', 'submitted', 'failed', 'timed_out',
                          'limit_reached', 'interrupted', 'cancelled')),
    detail            TEXT,
    progress          TEXT,    -- the last `flow_progress` line
    started_at        INTEGER NOT NULL,
    ended_at          INTEGER,
    UNIQUE (step_id, attempt)
);
CREATE INDEX invocations_by_flow ON invocations (flow_id);

-- What a run needs you to know or decide, kept until resolved.
CREATE TABLE inbox_items (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id      INTEGER NOT NULL REFERENCES flows (id),
    kind         TEXT    NOT NULL CHECK (kind IN ('failure', 'capacity', 'permission', 'interrupted')),
    subject      TEXT    NOT NULL,
    title        TEXT    NOT NULL,
    body         TEXT    NOT NULL,
    created_at   INTEGER NOT NULL,
    resolved_at  INTEGER
);

-- One open item per (flow, kind, subject), so a role that fails twice, or a limit reported by two
-- routes, is one item and one notification.
CREATE UNIQUE INDEX inbox_one_open ON inbox_items (flow_id, kind, subject)
    WHERE resolved_at IS NULL;
