-- Migration 20: the `[workflow]` each run reads, kept from when it began.
--
-- A run used to read your config afresh every time it was driven: at its start, and again when a
-- quit, a usage limit or an answered question picked it up. Settings › Recipes made editing that
-- config a click, and an edit would then have changed a run already under way — a role's skills or
-- effort mid-review, a planner resumed on another provider than the conversation it continues. So
-- a run keeps what it first read, as JSON, and every later drive reads that.
--
-- Per part, because an issue's run is steps that begin days apart: planning, designing and
-- implementing each keep the config as it was when that step began, so a step that has not begun
-- yet still sees an edit (`wtm_core::flow::RunPart`). A review is one part.
--
-- Written once per part and never changed: what a run read is part of its record.

CREATE TABLE run_workflows (
    flow_id      INTEGER NOT NULL REFERENCES flows (id),
    part         TEXT    NOT NULL CHECK (part IN ('review', 'plan', 'design', 'implement')),
    workflow     TEXT    NOT NULL, -- `WorkflowSpec`, as JSON
    captured_at  INTEGER NOT NULL,
    PRIMARY KEY (flow_id, part)
);

CREATE TRIGGER run_workflows_are_kept_as_written BEFORE UPDATE ON run_workflows
BEGIN
    SELECT RAISE(ABORT, 'a run keeps the workflow it started with');
END;
