-- T62: the schedule the daemon pulls loop sources on, and the per-source
-- record of what was already filed. One row per (source, key): a repeat pull
-- of the same item changes nothing.
CREATE TABLE schedule (
    name        TEXT PRIMARY KEY,
    interval_ms INTEGER NOT NULL CHECK (interval_ms > 0),
    next_due_ms INTEGER NOT NULL,
    last_run_ms INTEGER
);

CREATE TABLE source_cursor (
    source      TEXT NOT NULL,
    key         TEXT NOT NULL,
    proposal_id INTEGER NOT NULL REFERENCES proposal(id),
    ts_ms       INTEGER NOT NULL,
    PRIMARY KEY (source, key)
);
