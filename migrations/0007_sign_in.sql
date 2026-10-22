-- Email + password sign-in. One per account at most; the email is also the
-- subject of the account's ('email', ...) identity row.
CREATE TABLE passwords (
    account_id  INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    email       TEXT    NOT NULL UNIQUE COLLATE NOCASE,
    hash        TEXT    NOT NULL,       -- argon2id PHC string
    verified_at TEXT,                   -- NULL until the emailed link is opened
    created_at  TEXT    NOT NULL,
    changed_at  TEXT    NOT NULL
);

-- Single-use links sent by email. Only a SHA-256 of the token is stored, so a
-- copy of the database cannot be used to verify or reset anything.
CREATE TABLE email_tokens (
    token_hash  TEXT    PRIMARY KEY,
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    purpose     TEXT    NOT NULL CHECK (purpose IN ('verify', 'reset')),
    created_at  TEXT    NOT NULL,
    expires_at  TEXT    NOT NULL,
    used_at     TEXT
);
CREATE INDEX idx_email_tokens_expires ON email_tokens(expires_at);

-- Mails sent, so the app never goes past the mail service's free daily
-- limit. Rows older than two days are swept.
CREATE TABLE mail_log (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    purpose   TEXT NOT NULL,
    sent_at   TEXT NOT NULL
);
CREATE INDEX idx_mail_log_sent ON mail_log(sent_at);

-- A keyed hash of the address an account signed up from, for spotting
-- clusters of fake accounts; never the address itself. And when the username
-- last changed, since it may change once a month.
ALTER TABLE accounts ADD COLUMN signup_ip_hash TEXT;
ALTER TABLE accounts ADD COLUMN username_changed_at TEXT;
