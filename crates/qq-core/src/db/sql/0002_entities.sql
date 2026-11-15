-- Entities (R20) and the engine's workflow table. Foreign keys are enforced
-- by the opener (PRAGMA foreign_keys=ON).
CREATE TABLE project (
    id           INTEGER PRIMARY KEY,
    slug         TEXT NOT NULL UNIQUE,
    repo_path    TEXT,
    integ_branch TEXT,
    created_ms   INTEGER NOT NULL
);

CREATE TABLE agent (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    role       TEXT NOT NULL CHECK (role IN ('cos', 'supervisor', 'lead', 'peer')),
    kind       TEXT REFERENCES kind(name),
    project_id INTEGER REFERENCES project(id),
    spawner_id INTEGER REFERENCES agent(id),
    started_ms INTEGER NOT NULL,
    ended_ms   INTEGER
);
-- R8: a project has at most one active Lead.
CREATE UNIQUE INDEX agent_one_active_lead
    ON agent(project_id) WHERE role = 'lead' AND ended_ms IS NULL;

CREATE TABLE proposal (
    id                  INTEGER PRIMARY KEY,
    loop                INTEGER NOT NULL CHECK (loop IN (1, 2, 3)),
    risk                TEXT NOT NULL CHECK (risk IN ('low', 'high')),
    status              TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'approved', 'rejected')),
    project_id          INTEGER REFERENCES project(id),
    observation         TEXT,
    evidence            TEXT,
    suspected_mechanism TEXT,
    impact              TEXT,
    question            TEXT,
    recommendation      TEXT,
    created_ms          INTEGER NOT NULL
);

CREATE TABLE decision (
    id            INTEGER PRIMARY KEY,
    proposal_id   INTEGER NOT NULL REFERENCES proposal(id),
    decider_role  TEXT NOT NULL CHECK (decider_role IN ('cos', 'owner')),
    verdict       TEXT NOT NULL CHECK (verdict IN ('approve', 'reject')),
    note          TEXT,
    ts_ms         INTEGER NOT NULL
);

CREATE TABLE task (
    id          INTEGER PRIMARY KEY,
    project_id  INTEGER NOT NULL REFERENCES project(id),
    title       TEXT NOT NULL,
    kind        TEXT REFERENCES kind(name),
    disposition TEXT NOT NULL CHECK (disposition IN ('engineer', 'architect', 'reviewer', 'scout')),
    state       TEXT NOT NULL,
    proposal_id INTEGER REFERENCES proposal(id),
    created_ms  INTEGER NOT NULL
);

CREATE TABLE task_dep (
    task_id   INTEGER NOT NULL REFERENCES task(id),
    parent_id INTEGER NOT NULL REFERENCES task(id),
    PRIMARY KEY (task_id, parent_id),
    CHECK (task_id <> parent_id)
);

CREATE TABLE workflow (
    id          TEXT PRIMARY KEY,
    kind        TEXT NOT NULL,
    state       TEXT NOT NULL,
    project_id  INTEGER REFERENCES project(id),
    task_id     INTEGER REFERENCES task(id),
    started_ms  INTEGER NOT NULL,
    updated_ms  INTEGER NOT NULL,
    finished_ms INTEGER
);

CREATE TABLE rule (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    body        TEXT,
    source_path TEXT
);

CREATE TABLE skill (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    source_path TEXT,
    role_set    TEXT CHECK (role_set IN ('cos', 'lead', 'peer'))
);
