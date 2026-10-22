-- Migration 12: planning an issue. An Implement run's planning record, the plans its planner hands
-- in, what you decide about each, and the discussions of its questions.
--
-- The planner's sessions are the run's steps and invocations (migration 3), as a review role's are,
-- and its questions are migration 6's. What is new is only what a review never had: a plan with a
-- revision number, a decision about it, and a discussion before an answer.

-- One per Implement run that plans: the role, the worktree it reads, and what it was last given.
CREATE TABLE plannings (
    flow_id     INTEGER PRIMARY KEY REFERENCES flows (id),
    project     TEXT    NOT NULL,
    worktree    TEXT    NOT NULL, -- the worktree's id, which is its path
    issue_key   TEXT    NOT NULL,
    tracker     TEXT    NOT NULL,
    role        TEXT    NOT NULL,
    context     TEXT,             -- the PlanContext the drawer lists, as JSON
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

-- Every plan the planner handed in, numbered within its run. Never changed: an approval pins a
-- revision, and a revision that could be rewritten would make the pin mean nothing.
CREATE TABLE plans (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id         INTEGER NOT NULL REFERENCES flows (id),
    revision        INTEGER NOT NULL CHECK (revision >= 1),
    step_id         INTEGER REFERENCES steps (id),
    invocation_id   INTEGER REFERENCES invocations (id),
    plan            TEXT    NOT NULL, -- the PlanReport, as JSON
    issue           TEXT    NOT NULL, -- the IssueText the planner was given, as JSON
    issue_revision  TEXT    NOT NULL, -- that text's hash
    submitted_at    INTEGER NOT NULL,
    UNIQUE (flow_id, revision)
);

CREATE TRIGGER plans_are_never_updated BEFORE UPDATE ON plans
BEGIN
    SELECT RAISE(ABORT, 'plans is append-only');
END;

CREATE TRIGGER plans_are_never_deleted BEFORE DELETE ON plans
BEGIN
    SELECT RAISE(ABORT, 'plans is append-only');
END;

-- What you decided about a plan revision. A later decision is a later row.
--
-- `approval_id` names the approval an approved decision made, without a foreign key on purpose:
-- SQLite cannot change a CHECK in place, so a later migration that adds an approval intent rebuilds
-- `approvals`, and every table that refers to it has to be rebuilt in the same migration (migration
-- 11's notes). One referring table fewer is one fewer such rebuild; the store writes both rows in
-- one transaction, which is the guarantee the key would have given.
CREATE TABLE plan_decisions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id      INTEGER NOT NULL REFERENCES flows (id),
    plan_id      INTEGER NOT NULL REFERENCES plans (id),
    verdict      TEXT    NOT NULL CHECK (verdict IN ('approved', 'changes_requested')),
    approval_id  INTEGER,
    skip_design  INTEGER NOT NULL DEFAULT 0 CHECK (skip_design IN (0, 1)),
    note         TEXT,
    decided_at   INTEGER NOT NULL,
    CHECK ((verdict = 'approved') = (approval_id IS NOT NULL)),
    CHECK (verdict = 'approved' OR note IS NOT NULL)
);
CREATE INDEX plan_decisions_by_flow ON plan_decisions (flow_id);

CREATE TRIGGER plan_decisions_are_never_updated BEFORE UPDATE ON plan_decisions
BEGIN
    SELECT RAISE(ABORT, 'plan_decisions is append-only');
END;

CREATE TRIGGER plan_decisions_are_never_deleted BEFORE DELETE ON plan_decisions
BEGIN
    SELECT RAISE(ABORT, 'plan_decisions is append-only');
END;

-- Discuss: a question talked over with the role that asked it, before it is answered.
CREATE TABLE question_messages (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    question_id  INTEGER NOT NULL REFERENCES questions (id),
    speaker      TEXT    NOT NULL CHECK (speaker IN ('you', 'agent')),
    text         TEXT    NOT NULL,
    said_at      INTEGER NOT NULL
);
CREATE INDEX question_messages_by_question ON question_messages (question_id);

CREATE TRIGGER question_messages_are_never_updated BEFORE UPDATE ON question_messages
BEGIN
    SELECT RAISE(ABORT, 'question_messages is append-only');
END;

CREATE TRIGGER question_messages_are_never_deleted BEFORE DELETE ON question_messages
BEGIN
    SELECT RAISE(ABORT, 'question_messages is append-only');
END;
