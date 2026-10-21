-- Migration 2: review rules, their revisions, and which reviews read them.
--
-- A rule's content lives only in its revisions; the rules row holds what can change about the
-- rule as a whole: its status and approval, and what replaced it. Findings cite a rule id and a
-- revision number, so revisions and applications are append-only, enforced below, and a rule is
-- never deleted.

CREATE TABLE rules (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    project        TEXT    NOT NULL,
    origin         TEXT    NOT NULL CHECK (origin IN ('human', 'agent', 'imported')),
    status         TEXT    NOT NULL CHECK (status IN ('active', 'disabled', 'superseded')),
    approval       TEXT    NOT NULL CHECK (approval IN ('pending', 'approved', 'rejected')),
    source         TEXT    NOT NULL, -- the RuleSource, as JSON
    -- The flow it came out of, when it came out of one, so a rule can be traced to its work item.
    source_flow    INTEGER REFERENCES flows (id),
    superseded_by  INTEGER REFERENCES rules (id),
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    CHECK ((status = 'superseded') = (superseded_by IS NOT NULL)),
    -- Only an agent's proposal waits for approval. A rule you wrote or imported is approved by
    -- being saved, so nothing can write one of yours as pending or rejected, and nothing can write
    -- an approved rule as an agent's without the approval having gone through the rules first.
    CHECK (origin = 'agent' OR approval = 'approved')
);
CREATE INDEX rules_by_project ON rules (project);

CREATE TRIGGER rules_are_never_deleted BEFORE DELETE ON rules
BEGIN
    SELECT RAISE(ABORT, 'rules are never deleted; disable or replace one instead');
END;

CREATE TABLE rule_revisions (
    rule_id  INTEGER NOT NULL REFERENCES rules (id),
    number   INTEGER NOT NULL CHECK (number >= 1),
    draft    TEXT    NOT NULL, -- the RuleDraft, as JSON
    at       INTEGER NOT NULL,
    PRIMARY KEY (rule_id, number)
) WITHOUT ROWID;

-- Numbered 1, 2, 3 with no gaps, so "the latest" and "the next" are never ambiguous and a
-- revision a finding cites cannot be shadowed by a later insert under the same number.
CREATE TRIGGER rule_revisions_are_numbered_in_order BEFORE INSERT ON rule_revisions
WHEN NEW.number <> (SELECT COALESCE(MAX(number), 0) + 1 FROM rule_revisions WHERE rule_id = NEW.rule_id)
BEGIN
    SELECT RAISE(ABORT, 'rule revisions are numbered in order');
END;

CREATE TRIGGER rule_revisions_are_never_updated BEFORE UPDATE ON rule_revisions
BEGIN
    SELECT RAISE(ABORT, 'rule_revisions is append-only');
END;

CREATE TRIGGER rule_revisions_are_never_deleted BEFORE DELETE ON rule_revisions
BEGIN
    SELECT RAISE(ABORT, 'rule_revisions is append-only');
END;

-- The rule revisions each review invocation was given. An invocation is one role's attempt within
-- a flow; the invocations table itself arrives with the review engine, and when it does this table
-- is keyed the same way it will be.
CREATE TABLE rule_applications (
    flow_id     INTEGER NOT NULL REFERENCES flows (id),
    role        TEXT    NOT NULL,
    attempt     INTEGER NOT NULL CHECK (attempt >= 1),
    position    INTEGER NOT NULL, -- the order the role read them in
    rule_id     INTEGER NOT NULL,
    revision    INTEGER NOT NULL,
    reason      TEXT    NOT NULL, -- the MatchReason, as JSON
    applied_at  INTEGER NOT NULL,
    PRIMARY KEY (flow_id, role, attempt, rule_id),
    FOREIGN KEY (rule_id, revision) REFERENCES rule_revisions (rule_id, number)
) WITHOUT ROWID;
CREATE INDEX rule_applications_by_rule ON rule_applications (rule_id);

CREATE TRIGGER rule_applications_are_never_updated BEFORE UPDATE ON rule_applications
BEGIN
    SELECT RAISE(ABORT, 'rule_applications is append-only');
END;

CREATE TRIGGER rule_applications_are_never_deleted BEFORE DELETE ON rule_applications
BEGIN
    SELECT RAISE(ABORT, 'rule_applications is append-only');
END;
