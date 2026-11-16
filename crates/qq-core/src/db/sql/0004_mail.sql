-- Mail (T38). `recipient` and `sender` are agent names; a workflow with the
-- recipient's name, if one runs, is signalled on send. The cursor is the
-- per-recipient read position: delivery advances it with the read.
CREATE TABLE mail (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    sender     TEXT NOT NULL,
    recipient  TEXT NOT NULL,
    kind       TEXT NOT NULL DEFAULT 'note',
    body       TEXT NOT NULL CHECK (length(body) <= 4096),
    dedupe_key TEXT,
    created_ms INTEGER NOT NULL,
    UNIQUE (sender, dedupe_key)
);
CREATE INDEX mail_recipient ON mail(recipient, id);

CREATE TABLE mail_cursor (
    recipient TEXT PRIMARY KEY,
    mail_id   INTEGER NOT NULL
);
