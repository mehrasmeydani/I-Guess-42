-- "Ghost" guesses: numbers entered from /admin to see how a round behaves,
-- deliberately excluded from the public headcount and from deciding a winner.
-- Everything already in the table was a real entry, hence the default.
ALTER TABLE guesses ADD COLUMN participates INTEGER NOT NULL DEFAULT 1;

-- The winner query filters on this, so keep it in the covering index.
CREATE INDEX IF NOT EXISTS idx_guesses_round_participating
    ON guesses(round_date, participates, value);
