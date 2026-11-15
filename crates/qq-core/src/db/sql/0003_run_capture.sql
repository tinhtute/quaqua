-- Run capture (R19) and scores (R23).
CREATE TABLE run (
    id           INTEGER PRIMARY KEY,
    task_id      INTEGER REFERENCES task(id),
    agent_id     INTEGER NOT NULL REFERENCES agent(id),
    kind         TEXT NOT NULL REFERENCES kind(name),
    workflow_id  TEXT REFERENCES workflow(id),
    harness      TEXT NOT NULL,
    model        TEXT NOT NULL,
    effort       TEXT,
    candidate_sha TEXT,
    started_ms   INTEGER NOT NULL,
    ended_ms     INTEGER,
    outcome      TEXT,
    tokens_in    INTEGER,
    tokens_out   INTEGER,
    cost_usd     REAL,
    cost_state   TEXT NOT NULL CHECK (cost_state IN ('reported', 'unknown')),
    CHECK ((cost_state = 'unknown') = (cost_usd IS NULL))
);

CREATE TABLE run_transcript (
    run_id  INTEGER NOT NULL REFERENCES run(id),
    seq     INTEGER NOT NULL,
    ts_ms   INTEGER NOT NULL,
    role    TEXT NOT NULL,
    content TEXT NOT NULL,
    PRIMARY KEY (run_id, seq)
);

CREATE TABLE run_tool_call (
    id          INTEGER PRIMARY KEY,
    run_id      INTEGER NOT NULL REFERENCES run(id),
    seq         INTEGER NOT NULL,
    ts_ms       INTEGER NOT NULL,
    tool        TEXT NOT NULL,
    args_json   TEXT,
    result_json TEXT,
    ok          INTEGER CHECK (ok IN (0, 1))
);

CREATE TABLE run_artifact (
    id     INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES run(id),
    kind   TEXT NOT NULL,
    path   TEXT,
    sha    TEXT
);

CREATE TABLE score (
    id          INTEGER PRIMARY KEY,
    run_id      INTEGER NOT NULL REFERENCES run(id),
    rubric      TEXT NOT NULL,
    value       REAL NOT NULL,
    detail_json TEXT,
    ts_ms       INTEGER NOT NULL
);
