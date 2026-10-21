-- Migration 8: local fixes, and the leases that keep a writer and a review off the same worktree.
--
-- A lease is a row rather than a lock in memory, so a fix interrupted by a quit or a crash still
-- holds its worktree after the relaunch: the worktree may hold its half-done edits. A lease is
-- released once, with the reason, and never deleted, so the record says who had the worktree when.
--
-- The rule is the database's as well as the engine's: at most one open writer per worktree (the
-- partial unique index), and a writer never beside any other open lease, in either order (the
-- trigger). Two clicks racing for one worktree get one lease between them, whatever the code above
-- this did.

CREATE TABLE worktree_leases (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    worktree      TEXT    NOT NULL, -- the worktree's path, as its review run recorded it
    flow_id       INTEGER NOT NULL REFERENCES flows (id),
    kind          TEXT    NOT NULL CHECK (kind IN ('writer', 'reviewer')),
    holder        TEXT    NOT NULL, -- the writer's role, or the review's recipe
    acquired_at   INTEGER NOT NULL,
    released_at   INTEGER,
    released_why  TEXT    CHECK (released_why IN ('completed', 'cancelled', 'failed', 'recovered')),
    CHECK ((released_at IS NULL) = (released_why IS NULL))
);
CREATE UNIQUE INDEX one_open_writer_per_worktree ON worktree_leases (worktree)
    WHERE released_at IS NULL AND kind = 'writer';
CREATE INDEX open_leases ON worktree_leases (worktree) WHERE released_at IS NULL;

CREATE TRIGGER a_writer_never_shares_a_worktree BEFORE INSERT ON worktree_leases
WHEN NEW.released_at IS NULL AND EXISTS (
    SELECT 1 FROM worktree_leases
    WHERE worktree = NEW.worktree AND released_at IS NULL AND (kind = 'writer' OR NEW.kind = 'writer')
)
BEGIN
    SELECT RAISE(ABORT, 'the worktree is leased');
END;

CREATE TRIGGER a_released_lease_stays_released BEFORE UPDATE ON worktree_leases
WHEN OLD.released_at IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'a released lease stays released');
END;

CREATE TRIGGER leases_are_never_deleted BEFORE DELETE ON worktree_leases
BEGIN
    SELECT RAISE(ABORT, 'leases are never deleted');
END;

-- One local fix: the findings a writer was asked to fix, the files that held it to, and the
-- commits the app made for it. A second round on the same review is a second row; the writer's
-- step is the same one, run again, because a step is found by its role within a run.
CREATE TABLE fixes (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id     INTEGER NOT NULL REFERENCES flows (id),
    step_id     INTEGER NOT NULL REFERENCES steps (id),
    role        TEXT    NOT NULL,
    findings    TEXT    NOT NULL, -- the finding ids, as a JSON array
    scope       TEXT    NOT NULL, -- the files it may change, as JSON
    base_sha    TEXT    NOT NULL, -- the commit it starts from
    head_sha    TEXT,             -- its newest commit
    diff_hash   TEXT,             -- of `git diff --binary base head`, once it is ready
    state       TEXT    NOT NULL CHECK (state IN ('writing', 'ready', 'failed', 'cancelled')),
    detail      TEXT,
    asked       TEXT    NOT NULL DEFAULT '[]', -- questions about files outside the scope, as JSON
    allowed     TEXT    NOT NULL DEFAULT '[]', -- files you let in by answering one, as JSON
    started_at  INTEGER NOT NULL,
    ended_at    INTEGER,
    CHECK (state != 'ready' OR (head_sha IS NOT NULL AND diff_hash IS NOT NULL)),
    CHECK ((state = 'writing') = (ended_at IS NULL))
);
CREATE INDEX fixes_by_flow ON fixes (flow_id, id);
CREATE UNIQUE INDEX one_fix_writing_per_flow ON fixes (flow_id) WHERE state = 'writing';

CREATE TRIGGER a_finished_fix_stays_finished BEFORE UPDATE OF state ON fixes
WHEN OLD.state != 'writing' AND NEW.state != OLD.state
BEGIN
    SELECT RAISE(ABORT, 'a finished fix stays finished');
END;
