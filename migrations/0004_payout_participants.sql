-- Coalition points now go to everyone who played a round, not only its winner,
-- so a settled round records how many players were in that payout. Rows that
-- predate this only ever paid the winner, hence the default.
ALTER TABLE payouts ADD COLUMN participants INTEGER NOT NULL DEFAULT 1;
