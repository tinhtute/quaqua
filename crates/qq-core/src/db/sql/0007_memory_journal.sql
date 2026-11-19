-- R4: one row per memory file the `memory.write` verb writes, with the text
-- before and after. Append-only: the triggers refuse any UPDATE or DELETE.
CREATE TABLE memory_journal (
    id     INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms  INTEGER NOT NULL,
    caller TEXT NOT NULL,
    file   TEXT NOT NULL,
    before TEXT NOT NULL,
    after  TEXT NOT NULL
);
CREATE TRIGGER memory_journal_no_update BEFORE UPDATE ON memory_journal
BEGIN SELECT RAISE(ABORT, 'memory_journal is append-only'); END;
CREATE TRIGGER memory_journal_no_delete BEFORE DELETE ON memory_journal
BEGIN SELECT RAISE(ABORT, 'memory_journal is append-only'); END;
