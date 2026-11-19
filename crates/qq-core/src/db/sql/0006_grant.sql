-- R14/Q15: the full grant, repo tier and plan lane the gate reads. Written only
-- by the caller-checked `grant.*` daemon verbs; `grant_note` is the record of
-- each change and whether anh iu's herdr notification went out.
CREATE TABLE grant_full (
    repo       TEXT PRIMARY KEY,
    granted_ms INTEGER NOT NULL,
    granted_by TEXT NOT NULL
);
CREATE TABLE repo_tier (
    repo   TEXT PRIMARY KEY,
    tier   TEXT NOT NULL CHECK (tier IN ('low', 'medium', 'high')),
    set_ms INTEGER NOT NULL,
    set_by TEXT NOT NULL
);
CREATE TABLE plan_lane (
    lane   TEXT PRIMARY KEY,
    tier   TEXT NOT NULL CHECK (tier IN ('low', 'medium', 'high')),
    set_ms INTEGER NOT NULL,
    set_by TEXT NOT NULL
);
CREATE TABLE grant_note (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms    INTEGER NOT NULL,
    caller   TEXT NOT NULL,
    kind     TEXT NOT NULL,
    subject  TEXT NOT NULL,
    value    TEXT NOT NULL,
    notified INTEGER NOT NULL DEFAULT 0
);
