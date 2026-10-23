-- Migration 17: revisions, and what each round handed in.
--
-- A round can now be a revision you asked for after the hand-over, in your words: what testing it
-- showed, or a pull request's feedback. Its note is what it set out to do, so like a fix round's
-- findings it is never rewritten. Every round also keeps the writer's hand-in, written once with
-- its end, because the hand-over shows each round's summary and the writer's word on each
-- acceptance criterion, and the step's result holds only the last.
--
-- SQLite cannot change a CHECK in place, so `implement_rounds` is rebuilt. Nothing refers to it;
-- its ids and round numbers are kept.

CREATE TABLE implement_rounds_v17 (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id        INTEGER NOT NULL REFERENCES implementations (flow_id),
    round          INTEGER NOT NULL CHECK (round >= 1),
    kind           TEXT    NOT NULL CHECK (kind IN ('implement', 'fix', 'revise')),
    plan_revision  INTEGER NOT NULL CHECK (plan_revision >= 1),
    findings       TEXT    NOT NULL DEFAULT '[]', -- the findings it was asked to fix, as JSON
    checks         TEXT    NOT NULL DEFAULT '[]', -- the checks it was asked to make pass, as JSON
    note           TEXT,                          -- a revision's request, in your words
    head_sha       TEXT,
    changed        TEXT    NOT NULL DEFAULT '[]', -- the files it changed, as JSON
    handoff        TEXT,                          -- what the writer handed in, as JSON
    started_at     INTEGER NOT NULL,
    ended_at       INTEGER,
    UNIQUE (flow_id, round),
    CHECK (kind = 'fix' OR (findings = '[]' AND checks = '[]')),
    CHECK ((kind = 'revise') = (note IS NOT NULL AND length(trim(note)) > 0))
);

INSERT INTO implement_rounds_v17
    (id, flow_id, round, kind, plan_revision, findings, checks, note, head_sha, changed, handoff,
     started_at, ended_at)
SELECT id, flow_id, round, kind, plan_revision, findings, checks, NULL, head_sha, changed, NULL,
       started_at, ended_at
FROM implement_rounds;

DROP TABLE implement_rounds;
ALTER TABLE implement_rounds_v17 RENAME TO implement_rounds;

CREATE TRIGGER a_rounds_task_is_never_rewritten BEFORE UPDATE ON implement_rounds
WHEN OLD.ended_at IS NOT NULL
    OR NEW.findings != OLD.findings OR NEW.checks != OLD.checks OR NEW.kind != OLD.kind
    OR NEW.round != OLD.round OR NEW.plan_revision != OLD.plan_revision
    OR NEW.note IS NOT OLD.note
BEGIN
    SELECT RAISE(ABORT, 'a round ends once, and what it set out to do never changes');
END;

CREATE TRIGGER implement_rounds_are_never_deleted BEFORE DELETE ON implement_rounds
BEGIN
    SELECT RAISE(ABORT, 'implement_rounds is append-only');
END;
