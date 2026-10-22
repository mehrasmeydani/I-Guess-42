-- Nothing about the 42 round is shown to anyone who is not in it, and that
-- includes who is a 42 student: in the global round everyone goes by their
-- username alone.
DROP VIEW global_players;
CREATE VIEW global_players AS
SELECT id, username AS login, username AS display_name
FROM accounts;
