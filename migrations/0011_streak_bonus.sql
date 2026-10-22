-- A new kind of ledger row: 'streak', the 1000 points for every ten days in
-- a row played in the global round, claimed by the player. SQLite cannot
-- change a CHECK in place, so the table is rebuilt with every row kept,
-- ids included. Nothing references ledger rows, so nothing else moves.
CREATE TABLE ledger_new (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id  INTEGER NOT NULL REFERENCES accounts(id),
    round_date  TEXT    NOT NULL,
    kind        TEXT    NOT NULL CHECK (kind IN
                  ('signup', 'grant', 'stake', 'pot', 'bet', 'bet_win', 'refund', 'adjust', 'streak')),
    amount      INTEGER NOT NULL,
    ref         TEXT    NOT NULL,
    note        TEXT,
    created_at  TEXT    NOT NULL,
    UNIQUE (account_id, kind, ref)
);
INSERT INTO ledger_new (id, account_id, round_date, kind, amount, ref, note, created_at)
    SELECT id, account_id, round_date, kind, amount, ref, note, created_at FROM ledger;
DROP TABLE ledger;
ALTER TABLE ledger_new RENAME TO ledger;
CREATE INDEX idx_ledger_account ON ledger(account_id);
CREATE INDEX idx_ledger_round ON ledger(round_date, kind);
