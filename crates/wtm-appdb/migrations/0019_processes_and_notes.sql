-- Migration 19: the process behind each run session, and notes on a run that are not progress.
--
-- A run session's CLI is its own process group, so a crash of wtm leaves it running, possibly still
-- editing a worktree, with nothing that knows it is there. Its pid and start time are kept now, as
-- an environment's services' are (migration 10), so the next launch can find it and stop it — only
-- when both still match, because a pid alone may belong to something else by then. `pid` is
-- cleared once the launch has dealt with it.
--
-- A note is something that happened to a run while nothing in it moved: the Mac asleep, a session
-- the last wtm left running and this one stopped. It is not an event, so it never moves the run's
-- `updated_at`: sleeping is not working. Notes are kept as written.

ALTER TABLE invocations ADD COLUMN pid INTEGER;
ALTER TABLE invocations ADD COLUMN pid_started TEXT; -- the start time `ps` printed, compared, never parsed

CREATE TABLE flow_notes (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id   INTEGER NOT NULL REFERENCES flows (id),
    kind      TEXT    NOT NULL CHECK (kind IN ('asleep', 'reaped')),
    from_ms   INTEGER,
    to_ms     INTEGER,
    text      TEXT    NOT NULL,
    at        INTEGER NOT NULL,
    CHECK (kind != 'asleep' OR (from_ms IS NOT NULL AND to_ms IS NOT NULL AND from_ms <= to_ms))
);
CREATE INDEX flow_notes_by_flow ON flow_notes (flow_id, id);

CREATE TRIGGER flow_notes_are_kept_as_written BEFORE UPDATE ON flow_notes
BEGIN
    SELECT RAISE(ABORT, 'flow_notes are kept as written');
END;

CREATE TRIGGER flow_notes_are_never_deleted BEFORE DELETE ON flow_notes
BEGIN
    SELECT RAISE(ABORT, 'flow_notes are never deleted');
END;
