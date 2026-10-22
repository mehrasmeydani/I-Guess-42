-- Accounts whose username starts with "test" are the maintainers' own test
-- accounts (db::TEST_ACCOUNTS). Their guesses so far become ghosts, so they
-- drop out of results, trends, odds and the wins leaderboard; points already
-- paid stay where they are.
UPDATE global_guesses SET participates = 0
WHERE user_id IN (SELECT id FROM accounts WHERE username LIKE 'test%');
