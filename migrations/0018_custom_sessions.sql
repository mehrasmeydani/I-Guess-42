-- Custom sessions: a round of their own that a moderator opens for a set
-- time (a talk, an event, a class), joined by scanning its QR code. Same
-- rule as the daily rounds - lowest unique number wins. Players sign in
-- and play under their username, or play as a guest under a name of their
-- own for this session; a cookie remembers who is who either way.
CREATE TABLE custom_sessions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    code        TEXT    NOT NULL UNIQUE,    -- in the link and the QR code
    name        TEXT    NOT NULL,
    created_by  INTEGER REFERENCES accounts(id) ON DELETE SET NULL,
    created_at  TEXT    NOT NULL,
    ends_at     TEXT    NOT NULL            -- moved to now when ended early
);
CREATE INDEX idx_custom_sessions_ends ON custom_sessions(ends_at);

-- Who joined a session, by the token in their cookie: one token per
-- browser, the same in every session it joins. `account_id` is set for
-- those who joined signed in (their name is their username) and NULL for
-- guests. A name is unique in its session; the guess is NULL until they
-- lock one in, and final after.
CREATE TABLE custom_session_players (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  INTEGER NOT NULL REFERENCES custom_sessions(id) ON DELETE CASCADE,
    token       TEXT    NOT NULL,
    account_id  INTEGER REFERENCES accounts(id) ON DELETE SET NULL,
    name        TEXT    NOT NULL COLLATE NOCASE,
    joined_at   TEXT    NOT NULL,
    guess       INTEGER CHECK (guess >= 1),
    guessed_at  TEXT,
    UNIQUE (session_id, name),
    UNIQUE (session_id, token),
    UNIQUE (session_id, account_id)
);
-- The round page's buttons: the open sessions this browser, or this
-- account, is in.
CREATE INDEX idx_custom_session_players_token ON custom_session_players(token);
CREATE INDEX idx_custom_session_players_account ON custom_session_players(account_id);
