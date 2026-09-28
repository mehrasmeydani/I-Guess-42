use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::demo::{self, DemoGuess};
use crate::stats::{DayTally, Tally};

pub type Db = SqlitePool;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct User {
    pub id: i64,
    pub login: String,
    pub display_name: String,
    /// The intra avatar, refreshed at every sign-in. Kept although the
    /// terminal-style pages show no pictures.
    #[allow(dead_code)]
    pub image_url: Option<String>,
}

/// One closed round, with its winner if the round produced one.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RoundSummary {
    pub round_date: String,
    pub total: i64,
    pub winning_value: Option<i64>,
    pub winner_login: Option<String>,
    pub winner_name: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LeaderboardRow {
    pub login: String,
    pub display_name: String,
    pub wins: i64,
}

fn ts(t: DateTime<Utc>) -> String {
    // Fixed-width UTC, so string comparison in SQL matches chronological order.
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Pull the filesystem path out of a sqlite URL so we can create its directory.
fn sqlite_file_path(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))
        .unwrap_or(url);
    let rest = rest.split('?').next().unwrap_or(rest);
    if rest.is_empty() || rest == ":memory:" {
        None
    } else {
        Some(rest)
    }
}

pub async fn connect(url: &str) -> Result<Db> {
    if let Some(parent) = sqlite_file_path(url).map(Path::new).and_then(Path::parent) {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating database directory {}", parent.display()))?;
        }
    }

    let opts = SqliteConnectOptions::from_str(url)
        .with_context(|| format!("parsing DATABASE_URL {url}"))?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(opts)
        .await
        .context("opening the sqlite database")?;

    sqlx::migrate!()
        .run(&pool)
        .await
        .context("running migrations")?;
    Ok(pool)
}

// ---------------------------------------------------------------- users

pub async fn upsert_user(
    db: &Db,
    id: i64,
    login: &str,
    display_name: &str,
    image_url: Option<&str>,
) -> Result<()> {
    let now = ts(Utc::now());
    sqlx::query(
        "INSERT INTO users (id, login, display_name, image_url, created_at, last_seen_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)
         ON CONFLICT(id) DO UPDATE SET
             login        = excluded.login,
             display_name = excluded.display_name,
             image_url    = excluded.image_url,
             last_seen_at = excluded.last_seen_at",
    )
    .bind(id)
    .bind(login)
    .bind(display_name)
    .bind(image_url)
    .bind(&now)
    .execute(db)
    .await?;
    Ok(())
}

// ------------------------------------------------------------- sessions

pub async fn create_session(db: &Db, token: &str, user_id: i64, ttl_days: i64) -> Result<()> {
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO sessions (token, user_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
    )
    .bind(token)
    .bind(user_id)
    .bind(ts(now))
    .bind(ts(now + chrono::Duration::days(ttl_days)))
    .execute(db)
    .await?;
    Ok(())
}

pub async fn user_for_session(db: &Db, token: &str) -> Result<Option<User>> {
    let user = sqlx::query_as::<_, User>(
        "SELECT u.id, u.login, u.display_name, u.image_url
         FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token = ?1 AND s.expires_at > ?2",
    )
    .bind(token)
    .bind(ts(Utc::now()))
    .fetch_optional(db)
    .await?;
    Ok(user)
}

pub async fn delete_session(db: &Db, token: &str) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE token = ?1")
        .bind(token)
        .execute(db)
        .await?;
    Ok(())
}

/// Drop sessions and OAuth states that have aged out.
pub async fn purge_expired(db: &Db) -> Result<()> {
    let now = ts(Utc::now());
    sqlx::query("DELETE FROM sessions WHERE expires_at <= ?1")
        .bind(&now)
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM oauth_states WHERE expires_at <= ?1")
        .bind(&now)
        .execute(db)
        .await?;
    Ok(())
}

// --------------------------------------------------------- oauth states

pub async fn store_oauth_state(db: &Db, state: &str) -> Result<()> {
    let now = Utc::now();
    sqlx::query("INSERT INTO oauth_states (state, created_at, expires_at) VALUES (?1, ?2, ?3)")
        .bind(state)
        .bind(ts(now))
        .bind(ts(now + chrono::Duration::minutes(15)))
        .execute(db)
        .await?;
    Ok(())
}

/// Returns true if the state was present and unexpired. Single-use: the row is
/// deleted whether or not it had expired.
pub async fn consume_oauth_state(db: &Db, state: &str) -> Result<bool> {
    let now = ts(Utc::now());
    let rows = sqlx::query("DELETE FROM oauth_states WHERE state = ?1 AND expires_at > ?2")
        .bind(state)
        .bind(&now)
        .execute(db)
        .await?
        .rows_affected();
    Ok(rows > 0)
}

// ---------------------------------------------------------------- game

/// Records a guess. Returns `false` if this player already had one for the
/// round, leaving the original untouched.
///
/// Guesses are final, so this must never overwrite. `DO NOTHING` makes the
/// check and the insert one atomic statement: two requests racing each other
/// cannot both succeed, which a read-then-write pair could not guarantee.
pub async fn insert_guess(
    db: &Db,
    round_date: &str,
    user_id: i64,
    value: i64,
    participates: bool,
) -> Result<bool> {
    let inserted = sqlx::query(
        "INSERT INTO guesses (round_date, user_id, value, submitted_at, participates)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(round_date, user_id) DO NOTHING",
    )
    .bind(round_date)
    .bind(user_id)
    .bind(value)
    .bind(ts(Utc::now()))
    .bind(participates)
    .execute(db)
    .await?
    .rows_affected();
    Ok(inserted > 0)
}

/// Every guess in a round, with who made it. Admin/test use only - this is
/// exactly the information the game hides from players until a round closes.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct GuessRow {
    pub login: String,
    pub display_name: String,
    pub value: i64,
    pub participates: bool,
}

pub async fn round_guesses(db: &Db, round_date: &str) -> Result<Vec<GuessRow>> {
    let rows = sqlx::query_as::<_, GuessRow>(
        "SELECT u.login, u.display_name, g.value, g.participates
         FROM guesses g JOIN users u ON u.id = g.user_id
         WHERE g.round_date = ?1
         ORDER BY g.participates DESC, g.value ASC",
    )
    .bind(round_date)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

pub async fn clear_round(db: &Db, round_date: &str) -> Result<u64> {
    let n = sqlx::query("DELETE FROM guesses WHERE round_date = ?1")
        .bind(round_date)
        .execute(db)
        .await?
        .rows_affected();
    Ok(n)
}

/// Stand-in players you can sign in as: the negative-id users, minus the
/// generated `bot-###` crowd, which would bury them.
pub async fn demo_users(db: &Db) -> Result<Vec<User>> {
    let rows = sqlx::query_as::<_, User>(
        "SELECT id, login, display_name, image_url FROM users
         WHERE id < 0 AND login NOT LIKE 'bot-%' ORDER BY login",
    )
    .fetch_all(db)
    .await?;
    Ok(rows)
}

pub async fn user_by_login(db: &Db, login: &str) -> Result<Option<User>> {
    let user = sqlx::query_as::<_, User>(
        "SELECT id, login, display_name, image_url FROM users WHERE login = ?1",
    )
    .bind(login)
    .fetch_optional(db)
    .await?;
    Ok(user)
}

/// Creates a stand-in player for testing. Real 42 ids are positive, so
/// negative ids keep invented players trivially distinguishable from people.
pub async fn create_test_user(db: &Db, login: &str) -> Result<User> {
    let (lowest,): (Option<i64>,) = sqlx::query_as("SELECT MIN(id) FROM users")
        .fetch_one(db)
        .await?;
    let id = lowest.unwrap_or(0).min(0) - 1;
    let display_name = format!("{login} (test)");
    upsert_user(db, id, login, &display_name, None).await?;
    Ok(User {
        id,
        login: login.to_string(),
        display_name,
        image_url: None,
    })
}

// ------------------------------------------------------------ demo history

/// Writes generated history: creates any missing `bot-###` players (negative
/// ids, like every stand-in) and records their guesses. A bot that already
/// has a guess for a day keeps it, so running this twice tops up rather than
/// duplicates. One transaction, so it is quick and all-or-nothing.
pub async fn insert_demo(db: &Db, guesses: &[DemoGuess]) -> Result<u64> {
    let mut tx = db.begin().await?;
    let now = ts(Utc::now());

    let (lowest,): (Option<i64>,) = sqlx::query_as("SELECT MIN(id) FROM users")
        .fetch_one(&mut *tx)
        .await?;
    let mut next_id = lowest.unwrap_or(0).min(0) - 1;
    let mut ids = Vec::with_capacity(demo::BOTS);
    for i in 0..demo::BOTS {
        let login = demo::bot_login(i);
        let existing: Option<(i64,)> = sqlx::query_as("SELECT id FROM users WHERE login = ?1")
            .bind(&login)
            .fetch_optional(&mut *tx)
            .await?;
        let id = match existing {
            Some((id,)) if id < 0 => id,
            Some(_) => anyhow::bail!("{login} is a real account; refusing to use it as a bot"),
            None => {
                let id = next_id;
                next_id -= 1;
                sqlx::query(
                    "INSERT INTO users (id, login, display_name, image_url, created_at, last_seen_at)
                     VALUES (?1, ?2, ?3, NULL, ?4, ?4)",
                )
                .bind(id)
                .bind(&login)
                .bind(format!("Bot {:03}", i + 1))
                .bind(&now)
                .execute(&mut *tx)
                .await?;
                id
            }
        };
        ids.push(id);
    }

    let mut added = 0;
    for g in guesses {
        added += sqlx::query(
            "INSERT OR IGNORE INTO guesses (round_date, user_id, value, submitted_at, participates)
             VALUES (?1, ?2, ?3, ?4, 1)",
        )
        .bind(&g.round_date)
        .bind(ids[g.bot])
        .bind(g.value)
        .bind(&now)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    tx.commit().await?;
    Ok(added)
}

/// Deletes every bot, and with them (ON DELETE CASCADE) every guess they made.
pub async fn remove_demo(db: &Db) -> Result<u64> {
    let n = sqlx::query("DELETE FROM users WHERE id < 0 AND login LIKE 'bot-%'")
        .execute(db)
        .await?
        .rows_affected();
    Ok(n)
}

/// How many bots exist and how many guesses they hold.
pub async fn demo_counts(db: &Db) -> Result<(i64, i64)> {
    let counts: (i64, i64) = sqlx::query_as(
        "SELECT
             (SELECT COUNT(*) FROM users WHERE id < 0 AND login LIKE 'bot-%'),
             (SELECT COUNT(*) FROM guesses g JOIN users u ON u.id = g.user_id
              WHERE u.id < 0 AND u.login LIKE 'bot-%')",
    )
    .fetch_one(db)
    .await?;
    Ok(counts)
}

pub async fn my_guess(db: &Db, round_date: &str, user_id: i64) -> Result<Option<i64>> {
    let value: Option<(i64,)> =
        sqlx::query_as("SELECT value FROM guesses WHERE round_date = ?1 AND user_id = ?2")
            .bind(round_date)
            .bind(user_id)
            .fetch_optional(db)
            .await?;
    Ok(value.map(|v| v.0))
}

pub async fn guess_count(db: &Db, round_date: &str) -> Result<i64> {
    let (n,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM guesses WHERE round_date = ?1 AND participates = 1")
            .bind(round_date)
            .fetch_one(db)
            .await?;
    Ok(n)
}

/// The winner of a round is the *lowest value that exactly one player picked*.
/// A round with no such value has no winner.
const WINNER_CTE: &str = "
    WITH rounds AS (
        SELECT round_date, COUNT(*) AS total
        FROM guesses WHERE round_date < ?1 AND participates = 1
        GROUP BY round_date
    ),
    uniq AS (
        SELECT round_date, value
        FROM guesses WHERE round_date < ?1 AND participates = 1
        GROUP BY round_date, value
        HAVING COUNT(*) = 1
    ),
    winners AS (
        SELECT round_date, MIN(value) AS value FROM uniq GROUP BY round_date
    )
";

/// Closed rounds, newest first. `open_round` is the round still taking
/// guesses; everything strictly before it is closed.
pub async fn closed_rounds(db: &Db, open_round: &str, limit: i64) -> Result<Vec<RoundSummary>> {
    let sql = format!(
        "{WINNER_CTE}
         SELECT r.round_date, r.total,
                w.value    AS winning_value,
                u.login    AS winner_login,
                u.display_name AS winner_name
         FROM rounds r
         LEFT JOIN winners w ON w.round_date = r.round_date
         LEFT JOIN guesses g ON g.round_date = w.round_date AND g.value = w.value
                            AND g.participates = 1
         LEFT JOIN users   u ON u.id = g.user_id
         ORDER BY r.round_date DESC
         LIMIT ?2"
    );
    let rows = sqlx::query_as::<_, RoundSummary>(&sql)
        .bind(open_round)
        .bind(limit)
        .fetch_all(db)
        .await?;
    Ok(rows)
}

/// Summary for one specific closed round.
pub async fn round_summary(db: &Db, open_round: &str, date: &str) -> Result<Option<RoundSummary>> {
    let sql = format!(
        "{WINNER_CTE}
         SELECT r.round_date, r.total,
                w.value    AS winning_value,
                u.login    AS winner_login,
                u.display_name AS winner_name
         FROM rounds r
         LEFT JOIN winners w ON w.round_date = r.round_date
         LEFT JOIN guesses g ON g.round_date = w.round_date AND g.value = w.value
                            AND g.participates = 1
         LEFT JOIN users   u ON u.id = g.user_id
         WHERE r.round_date = ?2"
    );
    let row = sqlx::query_as::<_, RoundSummary>(&sql)
        .bind(open_round)
        .bind(date)
        .fetch_optional(db)
        .await?;
    Ok(row)
}

/// How many rounds have closed. A count rather than a list: the day page only
/// wants to say what its own short list leaves out.
pub async fn closed_round_count(db: &Db, open_round: &str) -> Result<i64> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT round_date) FROM guesses
         WHERE round_date < ?1 AND participates = 1",
    )
    .bind(open_round)
    .fetch_one(db)
    .await?;
    Ok(n)
}

/// The closed rounds just before `date`, newest first: the day page's own way
/// back through the calendar, so reading one round to the next never goes
/// through /results.
pub async fn rounds_before(
    db: &Db,
    open_round: &str,
    date: &str,
    limit: i64,
) -> Result<Vec<RoundSummary>> {
    let sql = format!(
        "{WINNER_CTE}
         SELECT r.round_date, r.total,
                w.value    AS winning_value,
                u.login    AS winner_login,
                u.display_name AS winner_name
         FROM rounds r
         LEFT JOIN winners w ON w.round_date = r.round_date
         LEFT JOIN guesses g ON g.round_date = w.round_date AND g.value = w.value
                            AND g.participates = 1
         LEFT JOIN users   u ON u.id = g.user_id
         WHERE r.round_date < ?2
         ORDER BY r.round_date DESC
         LIMIT ?3"
    );
    let rows = sqlx::query_as::<_, RoundSummary>(&sql)
        .bind(open_round)
        .bind(date)
        .bind(limit)
        .fetch_all(db)
        .await?;
    Ok(rows)
}

/// The closed round right after `date`, if `date` is not the newest one. Stays
/// below `open_round`, so today never leaks out as a link.
pub async fn round_after(db: &Db, open_round: &str, date: &str) -> Result<Option<String>> {
    let row: Option<Option<String>> = sqlx::query_scalar(
        "SELECT MIN(round_date) FROM guesses
         WHERE round_date > ?1 AND round_date < ?2 AND participates = 1",
    )
    .bind(date)
    .bind(open_round)
    .fetch_optional(db)
    .await?;
    Ok(row.flatten())
}

/// How many players picked each value in a round, lowest value first. Ghosts
/// are left out, as everywhere else in the game. Callers must only ask about
/// closed rounds - this does not check, and an open round's numbers are secret.
pub async fn round_tallies(db: &Db, round_date: &str) -> Result<Vec<Tally>> {
    let rows = sqlx::query_as::<_, Tally>(
        "SELECT value, COUNT(*) AS count
         FROM guesses WHERE round_date = ?1 AND participates = 1
         GROUP BY value ORDER BY value",
    )
    .bind(round_date)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Per-day tallies for every closed round from `from` (inclusive) up to the
/// open round (exclusive), oldest day first. Ghosts are left out. Stopping
/// before `open_round` is what keeps the open round's numbers secret.
pub async fn range_tallies(db: &Db, from: &str, open_round: &str) -> Result<Vec<DayTally>> {
    let rows = sqlx::query_as::<_, DayTally>(
        "SELECT round_date, value, COUNT(*) AS count
         FROM guesses
         WHERE round_date >= ?1 AND round_date < ?2 AND participates = 1
         GROUP BY round_date, value
         ORDER BY round_date, value",
    )
    .bind(from)
    .bind(open_round)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

pub async fn leaderboard(db: &Db, open_round: &str, limit: i64) -> Result<Vec<LeaderboardRow>> {
    let sql = format!(
        "{WINNER_CTE}
         SELECT u.login, u.display_name, COUNT(*) AS wins
         FROM winners w
         JOIN guesses g ON g.round_date = w.round_date AND g.value = w.value
                        AND g.participates = 1
         JOIN users   u ON u.id = g.user_id
         GROUP BY u.id
         ORDER BY wins DESC, u.login ASC
         LIMIT ?2"
    );
    let rows = sqlx::query_as::<_, LeaderboardRow>(&sql)
        .bind(open_round)
        .bind(limit)
        .fetch_all(db)
        .await?;
    Ok(rows)
}

// -------------------------------------------------------------- payouts

/// Everyone a closed round owes coalition points to: its winner, and every
/// real player who took part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payout {
    /// The winner's intra id.
    pub winner_id: i64,
    /// Every participant's intra id, lowest first, the winner among them.
    pub participant_ids: Vec<i64>,
}

/// What `round_date` owes, if that round is closed, has a winner, and has not
/// been settled yet. Stand-ins (negative ids) are left out throughout: they
/// are not 42 accounts and cannot receive points. A round whose winner is a
/// stand-in pays nobody, because the API wants a winner with every payout and
/// there is no honest id to name.
pub async fn unpaid_round(db: &Db, open_round: &str, round_date: &str) -> Result<Option<Payout>> {
    let sql = format!(
        "{WINNER_CTE}
         SELECT g.user_id
         FROM winners w
         JOIN guesses g ON g.round_date = w.round_date AND g.value = w.value
                        AND g.participates = 1
         WHERE w.round_date = ?2 AND g.user_id > 0
           AND NOT EXISTS (SELECT 1 FROM payouts p WHERE p.round_date = w.round_date)"
    );
    let row: Option<(i64,)> = sqlx::query_as(&sql)
        .bind(open_round)
        .bind(round_date)
        .fetch_optional(db)
        .await?;
    let Some((winner_id,)) = row else {
        return Ok(None);
    };

    let participants: Vec<(i64,)> = sqlx::query_as(
        "SELECT user_id FROM guesses
         WHERE round_date = ?1 AND participates = 1 AND user_id > 0
         ORDER BY user_id",
    )
    .bind(round_date)
    .fetch_all(db)
    .await?;
    Ok(Some(Payout {
        winner_id,
        participant_ids: participants.into_iter().map(|r| r.0).collect(),
    }))
}

/// Marks a round as settled, so its points are never sent a second time.
/// `outcome` is `given` or `already` (the API had paid out today already).
pub async fn record_payout(
    db: &Db,
    round_date: &str,
    payout: &Payout,
    outcome: &str,
    message: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO payouts (round_date, user_id, participants, outcome, message, paid_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(round_date) DO NOTHING",
    )
    .bind(round_date)
    .bind(payout.winner_id)
    .bind(payout.participant_ids.len() as i64)
    .bind(outcome)
    .bind(message)
    .bind(ts(Utc::now()))
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT: AtomicU32 = AtomicU32::new(0);

    /// A throwaway on-disk database. `:memory:` would give every pooled
    /// connection its own empty database, so a real file it is.
    struct TempDb {
        path: std::path::PathBuf,
        db: Db,
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let mut p = self.path.clone().into_os_string();
                p.push(suffix);
                let _ = std::fs::remove_file(p);
            }
        }
    }

    async fn temp_db() -> TempDb {
        let path = std::env::temp_dir().join(format!(
            "i_guess_42_test_{}_{}.db",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        for suffix in ["", "-wal", "-shm"] {
            let mut p = path.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(p);
        }
        let db = connect(&format!("sqlite://{}", path.display()))
            .await
            .unwrap();
        TempDb { path, db }
    }

    /// Register each player and record their guess for `round`.
    async fn seed(db: &Db, round: &str, guesses: &[(i64, i64)]) {
        for (user_id, value) in guesses {
            upsert_user(
                db,
                *user_id,
                &format!("p{user_id}"),
                &format!("Player {user_id}"),
                None,
            )
            .await
            .unwrap();
            insert_guess(db, round, *user_id, *value, true)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn the_lowest_unique_number_wins_not_the_lowest_number() {
        let t = temp_db().await;
        // Two people grabbed 1, so it is burned. 2 is the lowest survivor.
        seed(&t.db, "2026-09-05", &[(1, 1), (2, 1), (3, 2), (4, 3)]).await;

        let r = round_summary(&t.db, "2026-09-06", "2026-09-05")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.winning_value, Some(2));
        assert_eq!(r.winner_login.as_deref(), Some("p3"));
        assert_eq!(r.total, 4);
    }

    #[tokio::test]
    async fn a_round_with_no_unique_number_has_no_winner() {
        let t = temp_db().await;
        seed(&t.db, "2026-09-05", &[(1, 5), (2, 5), (3, 7), (4, 7)]).await;

        let r = round_summary(&t.db, "2026-09-06", "2026-09-05")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.winning_value, None);
        assert_eq!(r.winner_login, None);
        assert_eq!(r.total, 4);
    }

    #[tokio::test]
    async fn a_lone_player_wins_with_whatever_they_picked() {
        let t = temp_db().await;
        seed(&t.db, "2026-09-05", &[(1, 9_000_000_000)]).await;

        let r = round_summary(&t.db, "2026-09-06", "2026-09-05")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.winning_value, Some(9_000_000_000));
        assert_eq!(r.winner_login.as_deref(), Some("p1"));
    }

    #[tokio::test]
    async fn a_day_knows_the_rounds_around_it() {
        let t = temp_db().await;
        seed(&t.db, "2026-09-03", &[(1, 1), (2, 1)]).await; // nobody won
        seed(&t.db, "2026-09-04", &[(1, 2), (2, 3)]).await;
        seed(&t.db, "2026-09-05", &[(1, 4)]).await;
        seed(&t.db, "2026-09-06", &[(1, 5), (2, 6)]).await; // still open

        let open = "2026-09-06";
        let earlier = rounds_before(&t.db, open, "2026-09-05", 10).await.unwrap();
        let dates: Vec<&str> = earlier.iter().map(|r| r.round_date.as_str()).collect();
        assert_eq!(dates, ["2026-09-04", "2026-09-03"], "newest first, and not itself");
        assert_eq!(earlier[0].winning_value, Some(2));
        assert_eq!(earlier[1].winning_value, None, "every number collided that day");

        // The limit cuts the oldest rounds, not the nearest ones.
        let one = rounds_before(&t.db, open, "2026-09-05", 1).await.unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].round_date, "2026-09-04");

        assert_eq!(
            round_after(&t.db, open, "2026-09-04").await.unwrap().as_deref(),
            Some("2026-09-05")
        );
        // The open round is not a round to link to, and the first day has
        // nothing before it.
        assert_eq!(round_after(&t.db, open, "2026-09-05").await.unwrap(), None);
        assert!(rounds_before(&t.db, open, "2026-09-03", 10).await.unwrap().is_empty());

        assert_eq!(closed_round_count(&t.db, open).await.unwrap(), 3);
    }

    #[tokio::test]
    async fn the_open_round_is_never_scored() {
        let t = temp_db().await;
        seed(&t.db, "2026-09-06", &[(1, 1), (2, 2)]).await;

        assert!(round_summary(&t.db, "2026-09-06", "2026-09-06")
            .await
            .unwrap()
            .is_none());
        assert!(closed_rounds(&t.db, "2026-09-06", 10)
            .await
            .unwrap()
            .is_empty());
        // ...and it is still visible as an in-progress headcount.
        assert_eq!(guess_count(&t.db, "2026-09-06").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn a_guess_is_final_and_a_second_one_is_refused() {
        let t = temp_db().await;
        seed(&t.db, "2026-09-05", &[(1, 50)]).await;

        // The second attempt reports that nothing was written...
        assert!(!insert_guess(&t.db, "2026-09-05", 1, 3, true).await.unwrap());
        // ...and the original number is untouched.
        assert_eq!(my_guess(&t.db, "2026-09-05", 1).await.unwrap(), Some(50));
        assert_eq!(guess_count(&t.db, "2026-09-05").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn the_same_player_may_guess_again_in_a_later_round() {
        let t = temp_db().await;
        seed(&t.db, "2026-09-05", &[(1, 50)]).await;

        assert!(insert_guess(&t.db, "2026-09-06", 1, 3, true).await.unwrap());
        assert_eq!(my_guess(&t.db, "2026-09-05", 1).await.unwrap(), Some(50));
        assert_eq!(my_guess(&t.db, "2026-09-06", 1).await.unwrap(), Some(3));
    }

    #[tokio::test]
    async fn history_is_newest_first_and_the_leaderboard_tallies_wins() {
        let t = temp_db().await;
        seed(&t.db, "2026-09-03", &[(1, 1), (2, 2)]).await; // p1 wins
        seed(&t.db, "2026-09-04", &[(1, 4), (2, 4)]).await; // nobody wins
        seed(&t.db, "2026-09-05", &[(1, 8), (2, 3)]).await; // p2 wins
        seed(&t.db, "2026-09-06", &[(1, 1)]).await; // still open

        let rounds = closed_rounds(&t.db, "2026-09-06", 10).await.unwrap();
        let dates: Vec<&str> = rounds.iter().map(|r| r.round_date.as_str()).collect();
        assert_eq!(dates, ["2026-09-05", "2026-09-04", "2026-09-03"]);
        assert_eq!(rounds[1].winning_value, None);

        let board = leaderboard(&t.db, "2026-09-06", 10).await.unwrap();
        assert_eq!(board.len(), 2);
        assert!(board.iter().all(|r| r.wins == 1));
    }

    #[tokio::test]
    async fn tallies_count_each_value_once_per_round_and_skip_ghosts() {
        let t = temp_db().await;
        seed(&t.db, "2026-09-05", &[(1, 3), (2, 1), (3, 3)]).await;
        seed(&t.db, "2026-09-06", &[(1, 1)]).await; // another round
        upsert_user(&t.db, -1, "ghosty", "Ghosty", None)
            .await
            .unwrap();
        insert_guess(&t.db, "2026-09-05", -1, 2, false).await.unwrap();

        let tallies = round_tallies(&t.db, "2026-09-05").await.unwrap();
        assert_eq!(
            tallies,
            [Tally { value: 1, count: 1 }, Tally { value: 3, count: 2 }]
        );
    }

    #[tokio::test]
    async fn a_range_stops_before_the_open_round() {
        let t = temp_db().await;
        seed(&t.db, "2026-09-04", &[(1, 1)]).await; // before the range
        seed(&t.db, "2026-09-05", &[(1, 2), (2, 2)]).await;
        seed(&t.db, "2026-09-06", &[(1, 5)]).await;
        seed(&t.db, "2026-09-07", &[(1, 9)]).await; // still open

        let rows = range_tallies(&t.db, "2026-09-05", "2026-09-07").await.unwrap();
        let got: Vec<(&str, i64, i64)> = rows
            .iter()
            .map(|r| (r.round_date.as_str(), r.value, r.count))
            .collect();
        assert_eq!(got, [("2026-09-05", 2, 2), ("2026-09-06", 5, 1)]);
    }

    #[tokio::test]
    async fn demo_history_goes_in_and_comes_back_out_cleanly() {
        let t = temp_db().await;
        seed(&t.db, "2026-09-05", &[(1, 3)]).await; // a real player
        create_test_user(&t.db, "alice").await.unwrap(); // a hand-made stand-in

        let guesses = vec![
            DemoGuess { round_date: "2026-09-05".into(), bot: 0, value: 3 },
            DemoGuess { round_date: "2026-09-05".into(), bot: 1, value: 7 },
            DemoGuess { round_date: "2026-09-04".into(), bot: 0, value: 1 },
        ];
        assert_eq!(insert_demo(&t.db, &guesses).await.unwrap(), 3);
        // Running it again adds nothing: those bots already guessed those days.
        assert_eq!(insert_demo(&t.db, &guesses).await.unwrap(), 0);
        assert_eq!(demo_counts(&t.db).await.unwrap(), (demo::BOTS as i64, 3));

        // Bots stay out of the sign-in-as list.
        let listed: Vec<String> = demo_users(&t.db).await.unwrap().into_iter().map(|u| u.login).collect();
        assert_eq!(listed, ["alice"]);

        remove_demo(&t.db).await.unwrap();
        assert_eq!(demo_counts(&t.db).await.unwrap(), (0, 0));
        // The real player's guess and the hand-made stand-in survive.
        assert_eq!(guess_count(&t.db, "2026-09-05").await.unwrap(), 1);
        assert!(user_by_login(&t.db, "alice").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn ghosts_are_invisible_to_the_game() {
        let t = temp_db().await;
        // Real entries: 5 and 8. A ghost 1 would win outright if it counted.
        seed(&t.db, "2026-09-05", &[(1, 5), (2, 8)]).await;
        upsert_user(&t.db, -1, "ghosty", "Ghosty", None)
            .await
            .unwrap();
        assert!(insert_guess(&t.db, "2026-09-05", -1, 1, false)
            .await
            .unwrap());

        let r = round_summary(&t.db, "2026-09-06", "2026-09-05")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.winning_value, Some(5), "a ghost must not win");
        assert_eq!(r.total, 2, "a ghost must not be counted");
        assert_eq!(guess_count(&t.db, "2026-09-05").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn a_ghost_cannot_burn_a_real_number() {
        let t = temp_db().await;
        // Player 1 picks 3. A ghost also picks 3: if ghosts counted, 3 would be
        // duplicated and burned, and player 2 would win with 4 instead.
        seed(&t.db, "2026-09-05", &[(1, 3), (2, 4)]).await;
        upsert_user(&t.db, -1, "ghosty", "Ghosty", None)
            .await
            .unwrap();
        assert!(insert_guess(&t.db, "2026-09-05", -1, 3, false)
            .await
            .unwrap());

        let r = round_summary(&t.db, "2026-09-06", "2026-09-05")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.winning_value, Some(3));
        assert_eq!(r.winner_login.as_deref(), Some("p1"));
    }

    #[tokio::test]
    async fn ghosts_still_show_up_for_an_admin() {
        let t = temp_db().await;
        seed(&t.db, "2026-09-07", &[(1, 5)]).await;
        upsert_user(&t.db, -1, "ghosty", "Ghosty", None)
            .await
            .unwrap();
        insert_guess(&t.db, "2026-09-07", -1, 1, false)
            .await
            .unwrap();

        let rows = round_guesses(&t.db, "2026-09-07").await.unwrap();
        assert_eq!(rows.len(), 2);
        // Participating entries sort first.
        assert!(rows[0].participates);
        assert!(!rows[1].participates);
        assert_eq!(rows[1].value, 1);
    }

    #[tokio::test]
    async fn demo_users_are_the_negative_ids() {
        let t = temp_db().await;
        upsert_user(&t.db, 42, "real", "Real Person", None)
            .await
            .unwrap();
        let a = create_test_user(&t.db, "alice").await.unwrap();
        let b = create_test_user(&t.db, "bob").await.unwrap();
        assert!(a.id < 0 && b.id < 0 && a.id != b.id);

        let demos = demo_users(&t.db).await.unwrap();
        assert_eq!(demos.len(), 2);
        assert!(demos.iter().all(|u| u.id < 0));
        assert!(!demos.iter().any(|u| u.login == "real"));
    }

    #[tokio::test]
    async fn oauth_state_is_single_use() {
        let t = temp_db().await;
        store_oauth_state(&t.db, "abc").await.unwrap();

        assert!(consume_oauth_state(&t.db, "abc").await.unwrap());
        assert!(!consume_oauth_state(&t.db, "abc").await.unwrap());
        assert!(!consume_oauth_state(&t.db, "never-issued").await.unwrap());
    }

    #[tokio::test]
    async fn a_session_resolves_to_its_user_until_it_is_deleted() {
        let t = temp_db().await;
        upsert_user(&t.db, 7, "megardes", "Mehras", Some("https://cdn/x.jpg"))
            .await
            .unwrap();
        create_session(&t.db, "tok", 7, 30).await.unwrap();

        let u = user_for_session(&t.db, "tok").await.unwrap().unwrap();
        assert_eq!(u.login, "megardes");
        assert_eq!(u.image_url.as_deref(), Some("https://cdn/x.jpg"));

        delete_session(&t.db, "tok").await.unwrap();
        assert!(user_for_session(&t.db, "tok").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_expired_session_stops_resolving_and_gets_swept() {
        let t = temp_db().await;
        upsert_user(&t.db, 7, "megardes", "Mehras", None)
            .await
            .unwrap();
        create_session(&t.db, "stale", 7, -1).await.unwrap(); // expired yesterday

        assert!(user_for_session(&t.db, "stale").await.unwrap().is_none());
        purge_expired(&t.db).await.unwrap();
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sessions")
            .fetch_one(&t.db)
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn a_round_owes_its_winner_and_its_players_until_it_is_settled() {
        let t = temp_db().await;
        seed(&t.db, "2026-09-05", &[(11, 1), (12, 1), (13, 2)]).await;

        let owed = Payout {
            winner_id: 13,
            participant_ids: vec![11, 12, 13],
        };
        assert_eq!(
            unpaid_round(&t.db, "2026-09-06", "2026-09-05").await.unwrap(),
            Some(owed.clone())
        );
        record_payout(&t.db, "2026-09-05", &owed, "given", "ok").await.unwrap();
        assert_eq!(
            unpaid_round(&t.db, "2026-09-06", "2026-09-05").await.unwrap(),
            None
        );
        // Settling twice keeps the first record, with the headcount it paid.
        record_payout(&t.db, "2026-09-05", &owed, "already", "").await.unwrap();
        let (outcome, participants): (String, i64) =
            sqlx::query_as("SELECT outcome, participants FROM payouts WHERE round_date = '2026-09-05'")
                .fetch_one(&t.db)
                .await
                .unwrap();
        assert_eq!((outcome.as_str(), participants), ("given", 3));
    }

    #[tokio::test]
    async fn ghosts_and_stand_ins_are_not_paid_for_playing() {
        let t = temp_db().await;
        seed(&t.db, "2026-09-05", &[(11, 1), (12, 3)]).await;
        // A ghost guess from /admin, and a stand-in player: neither is a 42
        // account taking part in the round.
        upsert_user(&t.db, 14, "p14", "Player 14", None).await.unwrap();
        insert_guess(&t.db, "2026-09-05", 14, 9, false).await.unwrap();
        let bot = create_test_user(&t.db, "bot-001").await.unwrap();
        insert_guess(&t.db, "2026-09-05", bot.id, 7, true).await.unwrap();

        assert_eq!(
            unpaid_round(&t.db, "2026-09-06", "2026-09-05").await.unwrap(),
            Some(Payout {
                winner_id: 11,
                participant_ids: vec![11, 12],
            })
        );
    }

    #[tokio::test]
    async fn nobody_is_owed_points_for_an_open_round_a_draw_or_a_stand_in() {
        let t = temp_db().await;
        // Still open.
        seed(&t.db, "2026-09-06", &[(21, 1)]).await;
        assert_eq!(unpaid_round(&t.db, "2026-09-06", "2026-09-06").await.unwrap(), None);

        // No unique number.
        seed(&t.db, "2026-09-04", &[(22, 3), (23, 3)]).await;
        assert_eq!(unpaid_round(&t.db, "2026-09-06", "2026-09-04").await.unwrap(), None);

        // Won by a stand-in, which has no 42 account.
        let bot = create_test_user(&t.db, "bot-002").await.unwrap();
        insert_guess(&t.db, "2026-09-03", bot.id, 1, true).await.unwrap();
        assert_eq!(unpaid_round(&t.db, "2026-09-06", "2026-09-03").await.unwrap(), None);
    }

    #[test]
    fn extracts_the_file_path_from_sqlite_urls() {
        assert_eq!(
            sqlite_file_path("sqlite://data/game.db"),
            Some("data/game.db")
        );
        assert_eq!(
            sqlite_file_path("sqlite:data/game.db"),
            Some("data/game.db")
        );
        assert_eq!(
            sqlite_file_path("sqlite://game.db?mode=rwc"),
            Some("game.db")
        );
        assert_eq!(sqlite_file_path("sqlite::memory:"), None);
    }
}
