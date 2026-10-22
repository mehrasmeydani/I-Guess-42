-- A moderator's warning before a ban: when the account was warned and why.
-- The warning goes out by mail; this is the record that it did.
ALTER TABLE accounts ADD COLUMN warned_at TEXT;
ALTER TABLE accounts ADD COLUMN warn_reason TEXT;
