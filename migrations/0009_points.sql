-- In-game points for the global round: fake, for bragging only. They are
-- never bought, sold, transferred or exchanged for anything.
--
-- Every change to a balance, ever. A balance is SUM(amount); rows are never
-- updated or deleted, and a correction is a new row. UNIQUE (account, kind,
-- ref) is what makes every credit and charge happen at most once, however
-- often the code that writes it runs: the daily grant is keyed by the round,
-- a stake by the round, a pot by the round, a refund by what it refunds.
CREATE TABLE ledger (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id  INTEGER NOT NULL REFERENCES accounts(id),
    round_date  TEXT    NOT NULL,
    kind        TEXT    NOT NULL CHECK (kind IN
                  ('signup', 'grant', 'stake', 'pot', 'bet', 'bet_win', 'refund', 'adjust')),
    amount      INTEGER NOT NULL,
    ref         TEXT    NOT NULL,
    note        TEXT,
    created_at  TEXT    NOT NULL,
    UNIQUE (account_id, kind, ref)
);
CREATE INDEX idx_ledger_account ON ledger(account_id);
CREATE INDEX idx_ledger_round ON ledger(round_date, kind);

-- One row per settled global round: how big the pot was and where it went.
-- Its existence is what marks a round as settled.
CREATE TABLE pots (
    round_date   TEXT    PRIMARY KEY,
    stakes       INTEGER NOT NULL,      -- this round's guess stakes
    carried_in   INTEGER NOT NULL,      -- rolled over from rounds nobody won
    winner_id    INTEGER REFERENCES accounts(id),
    paid         INTEGER NOT NULL,      -- what the winner got; 0 if nobody won
    players      INTEGER NOT NULL,
    voided       INTEGER NOT NULL DEFAULT 0,   -- a moderator called it off
    settled_at   TEXT    NOT NULL
);
