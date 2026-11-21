-- T62c: the Supervisor jobs the daemon runs from its own loop. The first run of a job is one interval after
-- the migration, so a daemon that starts at once runs nothing at once.
-- `cursor_ms` is the scoring cursor: the newest run end the last scoring pass
-- covered. It starts at the newest run that already ended, so the history
-- before this migration is never scored. Source rows are added by the daemon
-- at start (`daemon::schedule::ensure_sources`).
ALTER TABLE schedule ADD COLUMN cursor_ms INTEGER;

INSERT OR IGNORE INTO schedule (name, interval_ms, next_due_ms, cursor_ms) VALUES
    ('supervisor.score', 3600000,
     CAST(strftime('%s', 'now') AS INTEGER) * 1000 + 3600000,
     (SELECT COALESCE(MAX(ended_ms), 0) FROM run)),
    ('supervisor.sensors', 21600000,
     CAST(strftime('%s', 'now') AS INTEGER) * 1000 + 21600000,
     NULL);
