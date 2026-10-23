-- Every time a run handed a role's work to another role or provider: by its fallback chain at a
-- limit or after a failure, or by you choosing to run it once elsewhere. Append-only, because a
-- switch is history: the Reviews tab says who ran each role and why, and the handover the next
-- runner was given is the evidence of what it was told.
CREATE TABLE provider_switches (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id    INTEGER NOT NULL REFERENCES flows (id),
    from_step  INTEGER NOT NULL REFERENCES steps (id),
    to_step    INTEGER NOT NULL REFERENCES steps (id),
    reason     TEXT    NOT NULL CHECK (reason IN ('limit', 'failure', 'once')),
    switched_by TEXT   NOT NULL CHECK (switched_by IN ('config', 'you')),
    handover   TEXT    NOT NULL, -- the Handover, as JSON
    at         INTEGER NOT NULL,
    -- Choosing to run a role once elsewhere is the only switch a person makes, and it keeps the
    -- role's own step; a fallback is always another role's step, chosen by the config.
    CHECK ((reason = 'once') = (switched_by = 'you')),
    CHECK ((reason = 'once') = (from_step = to_step))
);
CREATE INDEX provider_switches_by_flow ON provider_switches (flow_id);

CREATE TRIGGER provider_switches_are_never_updated BEFORE UPDATE ON provider_switches
BEGIN
    SELECT RAISE(ABORT, 'provider_switches is append-only');
END;

CREATE TRIGGER provider_switches_are_never_deleted BEFORE DELETE ON provider_switches
BEGIN
    SELECT RAISE(ABORT, 'provider_switches is append-only');
END;
