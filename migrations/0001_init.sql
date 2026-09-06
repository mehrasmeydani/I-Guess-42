-- 42 intra users who have logged in at least once.
CREATE TABLE IF NOT EXISTS users (
    id           INTEGER PRIMARY KEY,          -- the 42 intra user id
    login        TEXT    NOT NULL UNIQUE,
    display_name TEXT    NOT NULL,
    image_url    TEXT,
    created_at   TEXT    NOT NULL,
    last_seen_at TEXT    NOT NULL
);

-- Opaque server-side sessions. The cookie only carries `token`.
CREATE TABLE IF NOT EXISTS sessions (
    token      TEXT    PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TEXT    NOT NULL,
    expires_at TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at);

-- Short-lived OAuth `state` values, to pin the callback to the browser
-- that started the flow.
CREATE TABLE IF NOT EXISTS oauth_states (
    state      TEXT PRIMARY KEY,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

-- One guess per user per round. `round_date` is the Europe/Vienna date on
-- which the round's 12:42 deadline falls.
CREATE TABLE IF NOT EXISTS guesses (
    round_date   TEXT    NOT NULL,
    user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    value        INTEGER NOT NULL CHECK (value >= 1),
    submitted_at TEXT    NOT NULL,
    PRIMARY KEY (round_date, user_id)
);
CREATE INDEX IF NOT EXISTS idx_guesses_round_value ON guesses(round_date, value);
