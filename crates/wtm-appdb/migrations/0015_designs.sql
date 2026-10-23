-- Migration 15: designing an issue. An Implement run's design record, the revisions its designer
-- publishes, your comments on each, and what you decide about them.
--
-- The designer's sessions are the run's steps and invocations (migration 3), as the planner's are,
-- and an approval of a revision is migration 7's `approvals` row with the `design` intent, which
-- that table has allowed from the start. The artifacts themselves — each revision's HTML and reuse
-- notes — stay files under wtm's data directory, as Open Design's always have: a revision row names
-- its prototype and artifact, and pins the artifact's fingerprint, so a file changed on disk is
-- noticed rather than trusted.

-- One per Implement run that designs: the role, the worktree it reads, and why it designs.
CREATE TABLE designs (
    flow_id     INTEGER PRIMARY KEY REFERENCES flows (id),
    project     TEXT    NOT NULL,
    worktree    TEXT    NOT NULL, -- the worktree's id, which is its path
    issue_key   TEXT    NOT NULL,
    role        TEXT    NOT NULL,
    origin      TEXT    NOT NULL CHECK (origin IN ('plan', 'you')),
    request     TEXT,             -- what you asked for, when you asked for the design yourself
    prototype   TEXT,             -- the prototype its revisions are artifacts of, once prepared
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

-- Every revision the designer published, numbered within its run. Never changed and never removed:
-- an approval pins a revision, and every revision stays addressable however many came after it.
CREATE TABLE design_revisions (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id        INTEGER NOT NULL REFERENCES flows (id),
    revision       INTEGER NOT NULL CHECK (revision >= 1),
    prototype      TEXT    NOT NULL,
    review         TEXT    NOT NULL, -- the artifact's id within the prototype
    fingerprint    TEXT    NOT NULL, -- of the artifact's HTML and reuse notes together
    brief          TEXT    NOT NULL,
    feedback       TEXT,             -- what it answers: the note and comments on the one before
    reuse          TEXT    NOT NULL,
    sources        TEXT    NOT NULL, -- the SourcePins its evidence came from, as JSON
    plan_revision  INTEGER NOT NULL,
    step_id        INTEGER REFERENCES steps (id),
    invocation_id  INTEGER REFERENCES invocations (id),
    submitted_at   INTEGER NOT NULL,
    UNIQUE (flow_id, revision),
    UNIQUE (prototype, review)
);

CREATE TRIGGER design_revisions_are_never_updated BEFORE UPDATE ON design_revisions
BEGIN
    SELECT RAISE(ABORT, 'design_revisions is append-only');
END;

CREATE TRIGGER design_revisions_are_never_deleted BEFORE DELETE ON design_revisions
BEGIN
    SELECT RAISE(ABORT, 'design_revisions is append-only');
END;

-- A comment on one revision, such as "Header: use PageHeader". Sent to the designer with the next
-- request for changes to that revision, and kept beside it afterwards.
CREATE TABLE design_comments (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id      INTEGER NOT NULL REFERENCES flows (id),
    revision_id  INTEGER NOT NULL REFERENCES design_revisions (id),
    text         TEXT    NOT NULL CHECK (length(trim(text)) > 0),
    said_at      INTEGER NOT NULL
);
CREATE INDEX design_comments_by_flow ON design_comments (flow_id);

CREATE TRIGGER design_comments_are_never_updated BEFORE UPDATE ON design_comments
BEGIN
    SELECT RAISE(ABORT, 'design_comments is append-only');
END;

CREATE TRIGGER design_comments_are_never_deleted BEFORE DELETE ON design_comments
BEGIN
    SELECT RAISE(ABORT, 'design_comments is append-only');
END;

-- What you decided about a revision. A later decision is a later row. `approval_id` has no foreign
-- key, for the reason migration 13 gives for `plan_decisions`: one referring table fewer for the
-- migration that next rebuilds `approvals`. The store writes both rows in one transaction.
CREATE TABLE design_decisions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id      INTEGER NOT NULL REFERENCES flows (id),
    revision_id  INTEGER NOT NULL REFERENCES design_revisions (id),
    verdict      TEXT    NOT NULL CHECK (verdict IN ('approved', 'changes_requested')),
    approval_id  INTEGER,
    note         TEXT,
    decided_at   INTEGER NOT NULL,
    CHECK ((verdict = 'approved') = (approval_id IS NOT NULL)),
    CHECK (verdict = 'approved' OR note IS NOT NULL)
);
CREATE INDEX design_decisions_by_flow ON design_decisions (flow_id);

CREATE TRIGGER design_decisions_are_never_updated BEFORE UPDATE ON design_decisions
BEGIN
    SELECT RAISE(ABORT, 'design_decisions is append-only');
END;

CREATE TRIGGER design_decisions_are_never_deleted BEFORE DELETE ON design_decisions
BEGIN
    SELECT RAISE(ABORT, 'design_decisions is append-only');
END;
