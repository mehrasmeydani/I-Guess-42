-- Two lookups that read whole tables once there are thousands of players.
-- Finding the rounds with stakes and no pot yet, on every visit to /g/,
-- walks this one a round at a time instead of reading the whole ledger:
CREATE INDEX idx_ledger_kind_round ON ledger(kind, round_date);
-- A player's own days, read with every guess they make (streaks, catch-up)
-- and on their round page. The primary key starts with the round:
CREATE INDEX idx_global_guesses_user ON global_guesses(user_id, round_date);
