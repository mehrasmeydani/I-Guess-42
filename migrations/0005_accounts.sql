-- Everyone who can sign in, with ids of our own. Until now a signed-in person
-- was a 42 intra user and nothing else; accounts make room for other ways in.
-- A 42 student's account points at their `users` row through intra_id, and the
-- 42 pool keeps reading `users` and `guesses` exactly as before.
--
-- intra_id cascades: the only `users` rows ever deleted are stand-ins (demo
-- bots, test players), and their accounts and sessions go with them.
CREATE TABLE accounts (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    username      TEXT    NOT NULL UNIQUE COLLATE NOCASE,
    intra_id      INTEGER UNIQUE REFERENCES users(id) ON DELETE CASCADE,
    created_at    TEXT    NOT NULL,
    last_seen_at  TEXT    NOT NULL,
    banned_at     TEXT,
    ban_reason    TEXT,
    deleted_at    TEXT
);

-- One row per way of signing in. (provider, subject) is what the provider
-- calls this person: the intra id, the GitHub user id, Google's `sub`, or the
-- lowercased email for password logins.
CREATE TABLE identities (
    provider    TEXT    NOT NULL CHECK (provider IN ('42', 'github', 'google', 'email')),
    subject     TEXT    NOT NULL,
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    email       TEXT,
    created_at  TEXT    NOT NULL,
    PRIMARY KEY (provider, subject)
);
CREATE INDEX idx_identities_account ON identities(account_id);

-- Sessions belong to accounts now. Same opaque-token design as before.
CREATE TABLE account_sessions (
    token      TEXT    PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at TEXT    NOT NULL,
    expires_at TEXT    NOT NULL
);
CREATE INDEX idx_account_sessions_expires ON account_sessions(expires_at);

-- Every 42 user and hand-made stand-in gets an account, named after their
-- login. Generated demo bots only get one if someone is signed in as them;
-- the rest never sign in, and the bot crowd is deleted and regenerated freely.
INSERT INTO accounts (username, intra_id, created_at, last_seen_at)
SELECT login, id, created_at, last_seen_at FROM users
WHERE NOT (id < 0 AND login LIKE 'bot-%')
   OR id IN (SELECT user_id FROM sessions);

INSERT INTO identities (provider, subject, account_id, created_at)
SELECT '42', CAST(u.id AS TEXT), a.id, u.created_at
FROM users u JOIN accounts a ON a.intra_id = u.id
WHERE u.id > 0;

-- Live sessions move across with their tokens, so the release signs nobody out.
INSERT INTO account_sessions (token, account_id, created_at, expires_at)
SELECT s.token, a.id, s.created_at, s.expires_at
FROM sessions s JOIN accounts a ON a.intra_id = s.user_id;

DROP TABLE sessions;

-- Room for the sign-in flows that come next: which provider a state belongs
-- to, which account started it when it links a login instead of signing in,
-- and the PKCE verifier for providers that use one.
ALTER TABLE oauth_states ADD COLUMN provider TEXT NOT NULL DEFAULT '42';
ALTER TABLE oauth_states ADD COLUMN link_account_id INTEGER;
ALTER TABLE oauth_states ADD COLUMN pkce_verifier TEXT;
