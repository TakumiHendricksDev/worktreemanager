-- Migration 7: approvals, and the outbox's operations.
--
-- An approval is a fact: you allowed exactly this, at these revisions. It is never changed, and
-- approving the same thing again later is a later row. Its subject is kept whole, so the window can
-- show what you approved and the executor can check it against what would be sent.
--
-- An operation is one write to the world outside this machine, recorded before it is sent and
-- keyed by exactly what it sends, so the same write is never queued twice. It runs under the
-- latest approval that covered it, which is the one column besides its progress that moves.

CREATE TABLE approvals (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id     INTEGER NOT NULL REFERENCES flows (id),
    intent      TEXT    NOT NULL CHECK (intent IN (
                    'plan', 'design', 'publish', 'post', 'route', 'merge')),
    binding     TEXT    NOT NULL, -- the pins it covers, as a JSON object
    subject     TEXT    NOT NULL, -- what it was of, as JSON
    granted_at  INTEGER NOT NULL
);
CREATE INDEX approvals_by_flow ON approvals (flow_id);

CREATE TRIGGER approvals_are_never_updated BEFORE UPDATE ON approvals
BEGIN
    SELECT RAISE(ABORT, 'approvals is append-only');
END;

CREATE TRIGGER approvals_are_never_deleted BEFORE DELETE ON approvals
BEGIN
    SELECT RAISE(ABORT, 'approvals is append-only');
END;

CREATE TABLE operations (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id       INTEGER NOT NULL REFERENCES flows (id),
    kind          TEXT    NOT NULL,
    target        TEXT    NOT NULL, -- what it writes to, such as owner/name#40
    payload_hash  TEXT    NOT NULL,
    payload       TEXT    NOT NULL, -- exactly what it sends, as JSON; payload_hash is its hash
    approval_id   INTEGER NOT NULL REFERENCES approvals (id),
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
