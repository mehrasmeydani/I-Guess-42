-- Streaks now survive up to two missed days in a row (stats::FREEZE_DAYS).
-- That is worked out from the guesses, so it holds for every streak, past
-- ones too; but streak prizes are only offered under it from the day it
-- went live, recorded here. Days before keep the prizes they had.
CREATE TABLE rule_changes (
    name  TEXT PRIMARY KEY,
    since TEXT NOT NULL      -- a round date, YYYY-MM-DD
);
INSERT INTO rule_changes (name, since) VALUES ('streak_freeze', date('now'));
