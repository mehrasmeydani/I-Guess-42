-- A new kind of ledger row: 'catchup', the points a late joiner is owed so
-- they start level with the regulars who were there before them, paid a
-- part with each of their first days played (economy::catch_up). SQLite
-- cannot change a CHECK in place, so the table is rebuilt as in 0011, every
-- row kept, ids included.
CREATE TABLE ledger_new (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id  INTEGER NOT NULL REFERENCES accounts(id),
    round_date  TEXT    NOT NULL,
    kind        TEXT    NOT NULL CHECK (kind IN
                  ('signup', 'grant', 'stake', 'pot', 'bet', 'bet_win', 'refund', 'adjust', 'streak', 'catchup')),
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
