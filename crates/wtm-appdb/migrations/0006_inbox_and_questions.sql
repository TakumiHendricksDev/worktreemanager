-- Migration 6: the inbox as the one durable record of what runs need you for, and the questions it
-- carries.
--
-- Migration 3's inbox was keyed by (flow, kind, subject), with a subject that was a role's name or
-- the word "run", and an item was open or resolved. That was enough for the four things a review
-- could raise. It is not enough to send a click to the exact place an item is about, or to tell
-- "settled" from "put away". So an item now has a target — the run, a step, a finding, a question
-- or an approval — and a state: open, resolved or dismissed. A CHECK constraint cannot be altered,
-- so the table is rebuilt, and every existing item is carried over with its id.

-- A question a run asked you. The first answer is the one that counts; see the trigger below.
CREATE TABLE questions (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id         INTEGER NOT NULL REFERENCES flows (id),
    step_id         INTEGER REFERENCES steps (id),
    role            TEXT,
    text            TEXT    NOT NULL,
    why             TEXT    NOT NULL,
    unblocks        TEXT    NOT NULL,
    blocks          TEXT,             -- the Phase that waits for the answer; NULL when none does
    context         TEXT    NOT NULL, -- the ContextLinks, as a JSON list
    recommendation  TEXT,
    options         TEXT    NOT NULL, -- the options offered, as a JSON list of strings
    asked_at        INTEGER NOT NULL,
    answer_choice   INTEGER,          -- an index into options, when one was picked
    answer_text     TEXT,
    answered_at     INTEGER,
    CHECK ((answered_at IS NULL) = (answer_text IS NULL)),
    CHECK (answer_choice IS NULL OR answered_at IS NOT NULL)
);
CREATE INDEX questions_by_flow ON questions (flow_id);

-- Answering in two places must not answer twice: the answer that resumed something is the one the
-- record keeps.
CREATE TRIGGER questions_are_answered_once
BEFORE UPDATE OF answer_choice, answer_text, answered_at ON questions
WHEN OLD.answered_at IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'a question is answered once');
END;

CREATE TABLE inbox_items_v5 (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id      INTEGER NOT NULL REFERENCES flows (id),
    kind         TEXT    NOT NULL CHECK (kind IN (
                     'question', 'approval', 'failure', 'capacity', 'interrupted', 'review_ready')),
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

-- A provider's permission prompt was its own kind; it is the first of the approvals now. An item
-- whose subject names one of its run's roles was about that role's step, and every other item was
-- about the run: that is what migration 3's writers meant by the two subjects they used.
INSERT INTO inbox_items_v5
    (id, flow_id, kind, target_kind, target_id, subject, title, body, state, created_at, closed_at)
SELECT i.id,
       i.flow_id,
       CASE i.kind WHEN 'permission' THEN 'approval' ELSE i.kind END,
       CASE WHEN s.id IS NULL OR i.kind = 'interrupted' THEN 'flow' ELSE 'step' END,
       CASE WHEN s.id IS NULL OR i.kind = 'interrupted' THEN i.flow_id ELSE s.id END,
       i.subject,
       i.title,
       i.body,
       CASE WHEN i.resolved_at IS NULL THEN 'open' ELSE 'resolved' END,
       i.created_at,
       i.resolved_at
FROM inbox_items i
LEFT JOIN steps s ON s.flow_id = i.flow_id AND s.role = i.subject;

DROP TABLE inbox_items;
ALTER TABLE inbox_items_v5 RENAME TO inbox_items;

-- One open item per (target, kind): a role that fails twice, or a limit reported by two routes, is
-- one item and one notification. A closed item does not count, so the same thing happening again
-- after you dealt with it is news again.
CREATE UNIQUE INDEX inbox_one_open ON inbox_items (kind, target_kind, target_id)
    WHERE state = 'open';
CREATE INDEX inbox_by_flow ON inbox_items (flow_id);
