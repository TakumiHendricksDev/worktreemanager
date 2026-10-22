-- Checks that need services, and what a runtime role saw.
--
-- `blocked` joins a check's statuses: a check whose services were not answering when it was asked
-- for is recorded as that, never as a skip or a pass, with the services it found missing. Every
-- check keeps those services as evidence, as JSON in `services`: for one that ran, which were up
-- and at which address; for a blocked one, what was wrong with each.
--
-- SQLite cannot change a CHECK in place, so `check_runs` is rebuilt. Nothing refers to it, so the
-- copy, the drop and the rename need none of the care `approvals` took in migration 10. Its ids
-- are kept: a role cited them as `c<id>`.

CREATE TABLE check_runs_v11 (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id        INTEGER NOT NULL REFERENCES flows (id),
    check_id       TEXT    NOT NULL, -- its key in [workflow.check]
    step_id        INTEGER REFERENCES steps (id),
    invocation_id  INTEGER REFERENCES invocations (id),
    role           TEXT,
    status         TEXT    NOT NULL CHECK (status IN (
                       'passed', 'failed', 'timed_out', 'skipped', 'error', 'blocked')),
    detail         TEXT,
    exit_code      INTEGER,
    tree_sha       TEXT    NOT NULL,
    cwd            TEXT    NOT NULL,
    output_path    TEXT,    -- relative to the flow's directory
    output_sha256  TEXT,
    output_bytes   INTEGER,
    services       TEXT    NOT NULL DEFAULT '[]', -- the GatedService list, as JSON
    started_at     INTEGER NOT NULL,
    ended_at       INTEGER NOT NULL,
    -- A pass is an exit code of 0, and nothing else is: not a skip, not a block, not a role saying so.
    CHECK ((status = 'passed') = (exit_code IS 0)),
    CHECK (status NOT IN ('skipped', 'blocked') OR exit_code IS NULL),
    -- A blocked check says what it was waiting for.
    CHECK (status != 'blocked' OR (detail IS NOT NULL AND services != '[]'))
);
INSERT INTO check_runs_v11
    (id, flow_id, check_id, step_id, invocation_id, role, status, detail, exit_code, tree_sha, cwd,
     output_path, output_sha256, output_bytes, started_at, ended_at)
SELECT id, flow_id, check_id, step_id, invocation_id, role, status, detail, exit_code, tree_sha,
       cwd, output_path, output_sha256, output_bytes, started_at, ended_at
FROM check_runs;

DROP TABLE check_runs;
ALTER TABLE check_runs_v11 RENAME TO check_runs;
CREATE INDEX check_runs_by_flow ON check_runs (flow_id);

CREATE TRIGGER check_runs_are_never_updated BEFORE UPDATE ON check_runs
BEGIN
    SELECT RAISE(ABORT, 'check_runs is append-only');
END;

-- The pictures a runtime role took of the page it drove, and the one wtm took when the verified
-- preview first loaded. The PNG is a file under the flow's directory; this is its record, which a
-- finding cites as `s<id>` in its evidence. Append-only, like every other piece of evidence.
CREATE TABLE screenshots (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id        INTEGER NOT NULL REFERENCES flows (id),
    step_id        INTEGER REFERENCES steps (id),
    invocation_id  INTEGER REFERENCES invocations (id),
    role           TEXT,
    url            TEXT    NOT NULL,
    taken_by       TEXT    NOT NULL CHECK (taken_by IN ('wtm', 'role')),
    image_path     TEXT    NOT NULL, -- relative to the flow's directory
    image_sha256   TEXT    NOT NULL,
    image_bytes    INTEGER NOT NULL,
    taken_at       INTEGER NOT NULL
);
CREATE INDEX screenshots_by_flow ON screenshots (flow_id);

CREATE TRIGGER screenshots_are_never_updated BEFORE UPDATE ON screenshots
BEGIN
    SELECT RAISE(ABORT, 'screenshots is append-only');
END;
