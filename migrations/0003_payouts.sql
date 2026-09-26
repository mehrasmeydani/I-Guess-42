-- Coalition points paid to each round's winner through the 42 Vienna points
-- API. A row means the round is settled - paid now, or the API answered that
-- today's points were already given - so it is never sent twice. A failed
-- attempt leaves no row, so the next one tries again.
CREATE TABLE IF NOT EXISTS payouts (
    round_date TEXT    PRIMARY KEY,
    user_id    INTEGER NOT NULL,           -- the 42 intra user id
    outcome    TEXT    NOT NULL CHECK (outcome IN ('given', 'already')),
    message    TEXT    NOT NULL,           -- whatever the API said, for the logs
    paid_at    TEXT    NOT NULL
);
