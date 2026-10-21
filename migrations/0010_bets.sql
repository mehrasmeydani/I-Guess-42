-- Bets on how the global round will turn out (SPEC.md section 9). Points
-- only; a bet's stake and any win are ledger rows like everything else.
CREATE TABLE bets (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id   INTEGER NOT NULL REFERENCES accounts(id),
    round_date   TEXT    NOT NULL,
    market       TEXT    NOT NULL,      -- 'under:10', 'over:10', 'none'
    stake        INTEGER NOT NULL CHECK (stake > 0),
    odds_x100    INTEGER NOT NULL,      -- locked when placed: 2.35 is 235
    placed_at    TEXT    NOT NULL,
    result       TEXT    CHECK (result IN ('won', 'lost', 'void')),  -- NULL until settled
    payout       INTEGER,               -- what came back, stake included; 0 if lost
    settled_at   TEXT,
    UNIQUE (account_id, round_date, market)   -- one bet per market per round
);
CREATE INDEX idx_bets_round ON bets(round_date);
CREATE INDEX idx_bets_account ON bets(account_id, round_date);
