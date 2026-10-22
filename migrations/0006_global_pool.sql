-- The global pool: the same game, open to every account, with its own guesses.
-- Same shape and rules as `guesses`, so every query reads both the same way;
-- only here `user_id` is an *account* id, not an intra id. `participates`
-- works as in the 42 pool: 0 for admin ghost guesses and voided guesses.
CREATE TABLE global_guesses (
    round_date   TEXT    NOT NULL,
    user_id      INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    value        INTEGER NOT NULL CHECK (value >= 1),
    submitted_at TEXT    NOT NULL,
    participates INTEGER NOT NULL DEFAULT 1,
    ip_hash      TEXT,
    PRIMARY KEY (round_date, user_id)
);
CREATE INDEX idx_global_guesses_round ON global_guesses(round_date, participates, value);

-- Who plays the global pool, shaped like `users` (id, login, display_name) so
-- the queries that name a winner can join either. Everyone goes by their
-- username; a 42 student's carries a tag.
CREATE VIEW global_players AS
SELECT id,
       username AS login,
       CASE WHEN intra_id > 0 THEN username || ' (42)' ELSE username END AS display_name
FROM accounts;

-- When an account proved it can be reached: at once for 42, GitHub and Google
-- (they vouch for the person), on opening the emailed link for a password
-- account. Only verified accounts play the global pool. Everyone so far came
-- in through 42.
ALTER TABLE accounts ADD COLUMN verified_at TEXT;
UPDATE accounts SET verified_at = created_at;
