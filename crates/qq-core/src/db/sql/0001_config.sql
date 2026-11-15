-- Config: harnesses, kinds, profiles, role topology, flat settings.
CREATE TABLE harness (
    name              TEXT PRIMARY KEY,
    driver            TEXT NOT NULL CHECK (driver IN ('native', 'generic')),
    args_template     TEXT,
    resume_args       TEXT,
    herdr_agent_kind  TEXT,
    CHECK (name = lower(name) AND name <> 'omp')
);

CREATE TABLE kind (
    name        TEXT PRIMARY KEY,
    harness     TEXT NOT NULL REFERENCES harness(name),
    model       TEXT NOT NULL,
    effort      TEXT,
    fallback    TEXT REFERENCES kind(name),
    can_review  INTEGER NOT NULL DEFAULT 0 CHECK (can_review IN (0, 1)),
    spawn       TEXT NOT NULL DEFAULT 'auto' CHECK (spawn IN ('auto', 'approve')),
    on_cost_cap TEXT NOT NULL DEFAULT 'warn' CHECK (on_cost_cap IN ('warn', 'kill')),
    use_for     TEXT,
    avoid       TEXT
);

CREATE TABLE purpose (
    name TEXT PRIMARY KEY,
    kind TEXT NOT NULL REFERENCES kind(name)
);

CREATE TABLE kind_reviewer (
    position INTEGER PRIMARY KEY,
    kind     TEXT NOT NULL REFERENCES kind(name)
);

CREATE TABLE profile (
    name TEXT PRIMARY KEY
);

CREATE TABLE profile_kind (
    profile TEXT NOT NULL REFERENCES profile(name),
    kind    TEXT NOT NULL REFERENCES kind(name),
    harness TEXT REFERENCES harness(name),
    model   TEXT,
    effort  TEXT,
    PRIMARY KEY (profile, kind)
);

CREATE TABLE topology_edge (
    from_role TEXT NOT NULL CHECK (from_role IN ('cos', 'supervisor', 'lead', 'peer')),
    to_role   TEXT NOT NULL CHECK (to_role IN ('cos', 'supervisor', 'lead', 'peer')),
    edge      TEXT NOT NULL CHECK (edge IN ('spawn', 'mail', 'report', 'question')),
    PRIMARY KEY (from_role, to_role, edge)
);

CREATE TABLE setting (
    key        TEXT PRIMARY KEY,
    value_json TEXT NOT NULL CHECK (json_valid(value_json))
);

CREATE TABLE engine_default (
    key        TEXT PRIMARY KEY,
    value_json TEXT NOT NULL CHECK (json_valid(value_json))
);
