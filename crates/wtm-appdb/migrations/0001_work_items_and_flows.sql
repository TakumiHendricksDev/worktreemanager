-- Migration 1: work items, their links, flows and flow events.
--
-- Tables for questions, approvals, findings, rules and the outbox are not here. Each arrives in
-- its own numbered migration with the backlog item that first writes it, so no table exists
-- before something exercises it.
--
-- Every id is AUTOINCREMENT, which stops SQLite reusing the id of a deleted row. A flow's files
-- live in a directory named after its id, and a later flow must never inherit them.
--
-- Times are milliseconds since the Unix epoch, from the app's clock.

CREATE TABLE work_items (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    project     TEXT    NOT NULL,
    title       TEXT    NOT NULL,
    tracker     TEXT,
    issue_key   TEXT,
    repo        TEXT,
    pr_number   INTEGER,
    created_at  INTEGER NOT NULL,
    -- Half an identity would match nothing and block nothing.
    CHECK ((tracker IS NULL) = (issue_key IS NULL)),
    CHECK ((repo IS NULL) = (pr_number IS NULL)),
    -- One work item per issue, and one per pull request, an issue's current pull request
    -- included. SQLite treats NULLs as distinct here, so ad hoc items never collide.
    UNIQUE (tracker, issue_key),
    UNIQUE (repo, pr_number)
);

-- Every branch, worktree and pull request a work item has used, oldest first.
CREATE TABLE work_item_links (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    work_item_id  INTEGER NOT NULL REFERENCES work_items (id),
    kind          TEXT    NOT NULL CHECK (kind IN ('branch', 'worktree', 'pull_request')),
    link          TEXT    NOT NULL, -- the WorkLink, as JSON
    linked_at     INTEGER NOT NULL
);
CREATE INDEX work_item_links_by_item ON work_item_links (work_item_id);

-- The projection of flow_events: each row is what replaying its flow's events gives.
CREATE TABLE flows (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    work_item_id  INTEGER NOT NULL REFERENCES work_items (id),
    kind          TEXT    NOT NULL,
    phase         TEXT    NOT NULL,
    hold          TEXT,             -- the Hold, as JSON; NULL when nothing holds the flow
    outcome       TEXT CHECK (outcome IN ('done', 'failed', 'cancelled')), -- NULL while open
    failure       TEXT,             -- the reason, when the outcome is 'failed'
    last_seq      INTEGER NOT NULL,
    started_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    CHECK ((outcome IS 'failed') = (failure IS NOT NULL)),
    -- Nothing waits on a flow that is over.
    CHECK (outcome IS NULL OR hold IS NULL)
);

-- At most one open flow of each kind per work item, so a second Start finds the first one.
CREATE UNIQUE INDEX flows_one_open_per_kind ON flows (work_item_id, kind) WHERE outcome IS NULL;

CREATE TABLE flow_events (
    flow_id  INTEGER NOT NULL REFERENCES flows (id),
    seq      INTEGER NOT NULL,
    at       INTEGER NOT NULL,
    event    TEXT    NOT NULL, -- the FlowEvent, as JSON
    PRIMARY KEY (flow_id, seq)
) WITHOUT ROWID;

-- Append-only, enforced here rather than promised. A rewritten history would no longer be the
-- one its flows row was projected from.
CREATE TRIGGER flow_events_are_never_updated BEFORE UPDATE ON flow_events
BEGIN
    SELECT RAISE(ABORT, 'flow_events is append-only');
END;

CREATE TRIGGER flow_events_are_never_deleted BEFORE DELETE ON flow_events
BEGIN
    SELECT RAISE(ABORT, 'flow_events is append-only');
END;
