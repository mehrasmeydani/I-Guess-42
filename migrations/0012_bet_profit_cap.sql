-- The most a bet can win over its stake, locked when it is placed like its
-- odds: the average players a day times 100 (odds.rs). NULL for bets placed
-- before there was a cap, which pay in full.
ALTER TABLE bets ADD COLUMN max_profit INTEGER;
