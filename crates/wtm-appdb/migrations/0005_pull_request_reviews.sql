-- Migration 5: what wtm keeps about a teammate's pull request beyond its review runs.
--
-- None of it is a copy of GitHub's state except `mirror_prs`, which is the last reading and is
-- replaced by the next one. The rest is yours: a flag the forge has no field for, the heads a review
-- followed, and what you decided. Those last two are append-only, because a later head or a later
-- decision is new information rather than a correction of the old.

-- What the forge said about a work item's pull request the last time wtm read it.
CREATE TABLE mirror_prs (
    work_item_id  INTEGER PRIMARY KEY REFERENCES work_items (id),
    pull_request  TEXT    NOT NULL, -- the PullRequest, as JSON
    permission    TEXT    NOT NULL, -- yours on its base repository
    read_at       INTEGER NOT NULL
);

-- "I'm reviewing this". A row is the flag being on.
CREATE TABLE review_claims (
    work_item_id  INTEGER PRIMARY KEY REFERENCES work_items (id),
    claimed_at    INTEGER NOT NULL
);

-- A head a review read, recorded when Re-review changes moved it on to the next one.
CREATE TABLE reviewed_heads (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id      INTEGER NOT NULL REFERENCES flows (id),
    head_sha     TEXT    NOT NULL,
    snapshot_id  INTEGER NOT NULL REFERENCES snapshots (id),
    findings     TEXT    NOT NULL, -- JSON list of the finding ids that stood at that head
    checks       TEXT    NOT NULL, -- JSON list of the check run ids on its snapshot
    next_head    TEXT    NOT NULL,
    left_at      INTEGER NOT NULL
);
CREATE INDEX reviewed_heads_by_flow ON reviewed_heads (flow_id);

CREATE TRIGGER reviewed_heads_are_never_updated BEFORE UPDATE ON reviewed_heads
BEGIN
    SELECT RAISE(ABORT, 'reviewed_heads is append-only');
END;

CREATE TRIGGER reviewed_heads_are_never_deleted BEFORE DELETE ON reviewed_heads
BEGIN
    SELECT RAISE(ABORT, 'reviewed_heads is append-only');
END;

-- What you chose to do with a review. The newest row is the current choice.
CREATE TABLE review_outcomes (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id     INTEGER NOT NULL REFERENCES flows (id),
    kind        TEXT    NOT NULL CHECK (kind IN ('private')),
    head_sha    TEXT    NOT NULL, -- the head the review had read when you decided
    notes       TEXT    NOT NULL,
    findings    TEXT    NOT NULL, -- JSON list of the finding ids that stood then
    decided_at  INTEGER NOT NULL
);
CREATE INDEX review_outcomes_by_flow ON review_outcomes (flow_id);

CREATE TRIGGER review_outcomes_are_never_updated BEFORE UPDATE ON review_outcomes
BEGIN
    SELECT RAISE(ABORT, 'review_outcomes is append-only');
END;

CREATE TRIGGER review_outcomes_are_never_deleted BEFORE DELETE ON review_outcomes
BEGIN
    SELECT RAISE(ABORT, 'review_outcomes is append-only');
END;
