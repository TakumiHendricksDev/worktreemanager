-- Migration 18: the issue's own pull request's feedback.
--
-- Once the work is published, wtm follows its pull request — on demand and when the window comes
-- back into focus, never on a timer — and each review someone else leaves on it, asking for changes
-- or commenting, is kept here the first time it is read, with its inline comments. That is what
-- makes feedback news exactly once, and what a revision started from it takes in: `revised_in`
-- names the round that did, written once.
--
-- New feedback is its own inbox kind, so `inbox_items` is rebuilt for its CHECK: nothing refers to
-- it, so the copy, the drop and the rename need no more care than migration 12's `check_runs`. Its
-- ids are kept.

CREATE TABLE inbox_items_v18 (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id      INTEGER NOT NULL REFERENCES flows (id),
    kind         TEXT    NOT NULL CHECK (kind IN (
                     'question', 'approval', 'failure', 'capacity', 'interrupted', 'review_ready',
                     'feedback')),
    target_kind  TEXT    NOT NULL CHECK (target_kind IN (
                     'flow', 'step', 'finding', 'question', 'approval')),
    target_id    INTEGER NOT NULL, -- the step's, finding's, question's or approval's id; the flow's own for 'flow'
    subject      TEXT    NOT NULL, -- the target in words: a role's name, or "run"
    title        TEXT    NOT NULL,
    body         TEXT    NOT NULL,
    state        TEXT    NOT NULL CHECK (state IN ('open', 'resolved', 'dismissed')),
    created_at   INTEGER NOT NULL,
    closed_at    INTEGER,
    CHECK ((state = 'open') = (closed_at IS NULL)),
    CHECK (target_kind != 'flow' OR target_id = flow_id)
);

INSERT INTO inbox_items_v18
    (id, flow_id, kind, target_kind, target_id, subject, title, body, state, created_at, closed_at)
SELECT id, flow_id, kind, target_kind, target_id, subject, title, body, state, created_at, closed_at
FROM inbox_items;

DROP TABLE inbox_items;
ALTER TABLE inbox_items_v18 RENAME TO inbox_items;

CREATE UNIQUE INDEX inbox_one_open ON inbox_items (kind, target_kind, target_id)
    WHERE state = 'open';
CREATE INDEX inbox_by_flow ON inbox_items (flow_id);

CREATE TABLE pull_request_feedback (
    flow_id      INTEGER NOT NULL REFERENCES flows (id),
    review_id    INTEGER NOT NULL, -- the forge's id for the review
    author       TEXT,             -- its login; NULL for a deleted account
    state        TEXT    NOT NULL, -- the review's state as the forge said it
    body         TEXT    NOT NULL,
    url          TEXT    NOT NULL,
    commit_id    TEXT,             -- the head it was left on
    comments     TEXT    NOT NULL DEFAULT '[]', -- its inline comments, as JSON
    seen_at      INTEGER NOT NULL, -- when wtm first read it
    revised_in   INTEGER,          -- the round of the revision that took it in
    PRIMARY KEY (flow_id, review_id)
);

CREATE TRIGGER pull_request_feedback_is_kept_as_read BEFORE UPDATE ON pull_request_feedback
WHEN OLD.revised_in IS NOT NULL
    OR NEW.review_id != OLD.review_id OR NEW.body != OLD.body OR NEW.state != OLD.state
    OR NEW.comments != OLD.comments OR NEW.seen_at != OLD.seen_at
BEGIN
    SELECT RAISE(ABORT, 'feedback is kept as it was read, and taken into one revision');
END;

CREATE TRIGGER pull_request_feedback_is_never_deleted BEFORE DELETE ON pull_request_feedback
BEGIN
    SELECT RAISE(ABORT, 'pull_request_feedback is append-only');
END;
