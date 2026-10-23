-- Migration 16: implementing an issue. An Implement run's implementation, each round of writing it
-- did, the commits the app made for it, and what the questions it asked are about.
--
-- The writer's sessions are the run's steps and invocations (migration 3), as the planner's and the
-- designer's are, and its lease is migration 8's. The review its work gets is a review run of the
-- issue's own work item (migration 3's `review_runs`), named here, so its findings, checks and
-- reruns are the review engine's own; and its questions are migration 6's, with what each is about
-- kept beside them.

-- One per Implement run that implements: the writer, the scope the approved plan holds it to, and
-- how far fixing what the review finds may go, as your config said when it began.
CREATE TABLE implementations (
    flow_id        INTEGER PRIMARY KEY REFERENCES flows (id),
    project        TEXT    NOT NULL,
    worktree       TEXT    NOT NULL, -- the worktree's id, which is its path
    issue_key      TEXT    NOT NULL,
    role           TEXT    NOT NULL, -- the writer
    recipe         TEXT    NOT NULL, -- the review recipe its work gets
    base_sha       TEXT    NOT NULL, -- the worktree's HEAD when implementing began
    plan_revision  INTEGER NOT NULL CHECK (plan_revision >= 1),
    scope          TEXT    NOT NULL, -- the plan's areas and named files, as JSON
    fix_rounds     INTEGER NOT NULL CHECK (fix_rounds >= 0),
    fix_attempts   INTEGER NOT NULL CHECK (fix_attempts >= 1),
    review_flow    INTEGER REFERENCES flows (id),
    outcome        TEXT    CHECK (outcome IN ('ready', 'cancelled')),
    remaining      TEXT    NOT NULL DEFAULT '[]', -- what it handed over with, as JSON
    stopped        TEXT,                          -- why the loop stopped, as JSON
    started_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);
CREATE UNIQUE INDEX one_implementation_per_review ON implementations (review_flow)
    WHERE review_flow IS NOT NULL;

-- Handed over is not final: a plan change you agree to afterwards opens it again. Stopped is.
CREATE TRIGGER a_stopped_implementation_stays_stopped BEFORE UPDATE ON implementations
WHEN OLD.outcome = 'cancelled'
BEGIN
    SELECT RAISE(ABORT, 'a stopped implementation stays stopped');
END;

CREATE TRIGGER implementations_are_never_deleted BEFORE DELETE ON implementations
BEGIN
    SELECT RAISE(ABORT, 'implementations are never deleted');
END;

-- Each round of writing, numbered within its run: implementing a plan revision, or fixing what a
-- review found. What a fix round set out to fix is what the bounds count, so it is never changed;
-- only its end is written, once.
CREATE TABLE implement_rounds (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id        INTEGER NOT NULL REFERENCES implementations (flow_id),
    round          INTEGER NOT NULL CHECK (round >= 1),
    kind           TEXT    NOT NULL CHECK (kind IN ('implement', 'fix')),
    plan_revision  INTEGER NOT NULL CHECK (plan_revision >= 1),
    findings       TEXT    NOT NULL DEFAULT '[]', -- the findings it was asked to fix, as JSON
    checks         TEXT    NOT NULL DEFAULT '[]', -- the checks it was asked to make pass, as JSON
    head_sha       TEXT,
    changed        TEXT    NOT NULL DEFAULT '[]', -- the files it changed, as JSON
    started_at     INTEGER NOT NULL,
    ended_at       INTEGER,
    UNIQUE (flow_id, round),
    CHECK (kind = 'fix' OR (findings = '[]' AND checks = '[]'))
);

CREATE TRIGGER a_rounds_task_is_never_rewritten BEFORE UPDATE ON implement_rounds
WHEN OLD.ended_at IS NOT NULL
    OR NEW.findings != OLD.findings OR NEW.checks != OLD.checks OR NEW.kind != OLD.kind
    OR NEW.round != OLD.round OR NEW.plan_revision != OLD.plan_revision
BEGIN
    SELECT RAISE(ABORT, 'a round ends once, and what it set out to do never changes');
END;

CREATE TRIGGER implement_rounds_are_never_deleted BEFORE DELETE ON implement_rounds
BEGIN
    SELECT RAISE(ABORT, 'implement_rounds is append-only');
END;

-- Every commit the app made for an implementation: the writer's checkpoints, and what it left
-- within the scope when it handed in.
CREATE TABLE implement_checkpoints (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id     INTEGER NOT NULL REFERENCES implementations (flow_id),
    round       INTEGER NOT NULL,
    commit_sha  TEXT    NOT NULL,
    message     TEXT    NOT NULL,
    paths       TEXT    NOT NULL, -- as JSON
    made_by     TEXT    NOT NULL CHECK (made_by IN ('writer', 'app')),
    made_at     INTEGER NOT NULL
);
CREATE INDEX implement_checkpoints_by_flow ON implement_checkpoints (flow_id, id);

CREATE TRIGGER implement_checkpoints_are_never_updated BEFORE UPDATE ON implement_checkpoints
BEGIN
    SELECT RAISE(ABORT, 'implement_checkpoints is append-only');
END;

CREATE TRIGGER implement_checkpoints_are_never_deleted BEFORE DELETE ON implement_checkpoints
BEGIN
    SELECT RAISE(ABORT, 'implement_checkpoints is append-only');
END;

-- What a question an implementation asked is about: files outside the plan the writer changed,
-- findings outside it, or the writer's own question. Its answer does what this says.
CREATE TABLE implement_questions (
    question_id    INTEGER PRIMARY KEY REFERENCES questions (id),
    flow_id        INTEGER NOT NULL REFERENCES implementations (flow_id),
    kind           TEXT    NOT NULL CHECK (kind IN ('outliers', 'findings', 'writer')),
    paths          TEXT    NOT NULL DEFAULT '[]', -- as JSON
    findings       TEXT    NOT NULL DEFAULT '[]', -- as JSON
    plan_revision  INTEGER NOT NULL
);
CREATE INDEX implement_questions_by_flow ON implement_questions (flow_id);

CREATE TRIGGER implement_questions_are_never_updated BEFORE UPDATE ON implement_questions
BEGIN
    SELECT RAISE(ABORT, 'implement_questions is append-only');
END;

CREATE TRIGGER implement_questions_are_never_deleted BEFORE DELETE ON implement_questions
BEGIN
    SELECT RAISE(ABORT, 'implement_questions is append-only');
END;
