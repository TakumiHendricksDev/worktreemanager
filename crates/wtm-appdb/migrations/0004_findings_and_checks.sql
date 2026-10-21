-- Migration 4: a review's consolidated findings, the reports each was made of, and the checks the
-- app ran for its roles.
--
-- Findings are saved rather than worked out on every read because they have identities other
-- things point at: a dismissal, a rule saved from one (its revision records the finding's id), and
-- the window. A consolidation after a rerun supersedes the findings it no longer reports and keeps
-- the rows, so a rule's provenance still resolves after the code moved on.

CREATE TABLE findings (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id           INTEGER NOT NULL REFERENCES flows (id),
    class             TEXT    NOT NULL, -- the FindingClass as JSON, with what that class requires
    severity          TEXT    NOT NULL CHECK (severity IN ('minor', 'notable', 'blocking')),
    path              TEXT,
    start_line        INTEGER,
    end_line          INTEGER,
    lines             INTEGER NOT NULL, -- 0 when no report named lines: a finding about a whole file
    title             TEXT    NOT NULL,
    rationale         TEXT    NOT NULL,
    requirement       TEXT,
    tree_sha          TEXT    NOT NULL, -- the tree its leading report was found on
    dismissed_reason  TEXT,
    dismissed_at      INTEGER,
    created_at        INTEGER NOT NULL,
    superseded_at     INTEGER,          -- a later consolidation stopped reporting it as it was
    CHECK ((path IS NULL) = (start_line IS NULL) AND (path IS NULL) = (end_line IS NULL)),
    CHECK ((dismissed_reason IS NULL) = (dismissed_at IS NULL))
);
CREATE INDEX findings_by_flow ON findings (flow_id);

-- The reports a finding was made of, exactly as each role submitted them.
CREATE TABLE finding_evidence (
    finding_id     INTEGER NOT NULL REFERENCES findings (id),
    invocation_id  INTEGER NOT NULL REFERENCES invocations (id),
    ordinal        INTEGER NOT NULL, -- its position in that invocation's submission
    step_id        INTEGER NOT NULL REFERENCES steps (id),
    role           TEXT    NOT NULL,
    provider       TEXT    NOT NULL,
    model          TEXT,
    workspace      TEXT    NOT NULL,
    tree_sha       TEXT    NOT NULL,
    reported       TEXT    NOT NULL, -- the ReportedFinding as JSON
    PRIMARY KEY (finding_id, invocation_id, ordinal)
);

-- What a role said is evidence, and evidence is not edited after the fact.
CREATE TRIGGER finding_evidence_is_never_updated BEFORE UPDATE ON finding_evidence
BEGIN
    SELECT RAISE(ABORT, 'finding_evidence is append-only');
END;

-- One run of a configured check, at a role's request.
CREATE TABLE check_runs (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id        INTEGER NOT NULL REFERENCES flows (id),
    check_id       TEXT    NOT NULL, -- its key in [workflow.check]
    step_id        INTEGER REFERENCES steps (id),
    invocation_id  INTEGER REFERENCES invocations (id),
    role           TEXT,
    status         TEXT    NOT NULL CHECK (status IN (
                       'passed', 'failed', 'timed_out', 'skipped', 'error')),
    detail         TEXT,
    exit_code      INTEGER,
    tree_sha       TEXT    NOT NULL,
    cwd            TEXT    NOT NULL,
    output_path    TEXT,    -- relative to the flow's directory
    output_sha256  TEXT,
    output_bytes   INTEGER,
    started_at     INTEGER NOT NULL,
    ended_at       INTEGER NOT NULL,
    -- A pass is an exit code of 0, and nothing else is: not a skip, not a role saying so.
    CHECK ((status = 'passed') = (exit_code IS 0)),
    CHECK (status != 'skipped' OR exit_code IS NULL)
);
CREATE INDEX check_runs_by_flow ON check_runs (flow_id);

CREATE TRIGGER check_runs_are_never_updated BEFORE UPDATE ON check_runs
BEGIN
    SELECT RAISE(ABORT, 'check_runs is append-only');
END;

-- The repository instruction files an invocation's brief carried, as a JSON list of paths.
ALTER TABLE invocations ADD COLUMN instruction_sources TEXT;

-- Why a step cannot run at all for want of a skill, as the typed problem's JSON.
ALTER TABLE steps ADD COLUMN skill_problem TEXT;
