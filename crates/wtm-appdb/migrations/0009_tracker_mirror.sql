-- Migration 9: the tracker's side of the Work view, as the last sync read it.
--
-- Mirrors only. Every row is what the tracker said, replaced by the next reading. Nothing in this
-- file refers to a work item, a flow, a step or an invocation, and nothing refers to it: a sync
-- writes these tables and no others, which is how "an issue moved in the tracker starts nothing"
-- holds at the bottom of the stack as well as in the use-case's signature
-- (`wtm_core::usecase::tracker_sync`).

-- Each issue as last read, keyed the way work items key issues: (tracker, key).
CREATE TABLE mirror_issues (
    tracker         TEXT    NOT NULL,
    issue_key       TEXT    NOT NULL,
    status          TEXT    NOT NULL, -- the team's name for it, for someone reading the file
    issue           TEXT    NOT NULL, -- the Issue a search or a read returned, as JSON
    read_at         INTEGER NOT NULL,
    detail          TEXT,             -- the IssueDetail, as JSON, once the issue has been opened
    detail_read_at  INTEGER,
    PRIMARY KEY (tracker, issue_key)
);

-- Sprints as last read, with their goals.
CREATE TABLE mirror_sprints (
    tracker    TEXT    NOT NULL,
    sprint_id  INTEGER NOT NULL,
    sprint     TEXT    NOT NULL, -- the Sprint, as JSON
    read_at    INTEGER NOT NULL,
    PRIMARY KEY (tracker, sprint_id)
);

-- Each project's queue, and how its last sync went: the last success (`read_at` and what it read)
-- and, if the last attempt failed after it, when and why.
CREATE TABLE mirror_queues (
    project     TEXT    PRIMARY KEY,
    tracker     TEXT    NOT NULL,
    query       TEXT,                       -- NULL until a sync has succeeded
    board       INTEGER,
    sprint_ids  TEXT    NOT NULL DEFAULT '[]', -- the board's active sprints, as a JSON list
    read_at     INTEGER,
    failed_at   INTEGER,
    failure     TEXT
);

-- A queue's issues, in the tracker's order.
CREATE TABLE mirror_queue_issues (
    project    TEXT    NOT NULL REFERENCES mirror_queues (project) ON DELETE CASCADE,
    position   INTEGER NOT NULL,
    tracker    TEXT    NOT NULL,
    issue_key  TEXT    NOT NULL,
    sprint_id  INTEGER,
    PRIMARY KEY (project, position),
    UNIQUE (project, issue_key)
);
