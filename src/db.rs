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

/// Someone who can sign in. Everyone has one, whichever way they came in; a
/// 42 student's also points at their `users` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: i64,
    pub username: String,
    /// The `users` row this account plays the 42 pool as. Set for 42 students
    /// and the stand-ins of a test instance, `None` for everyone else.
    pub intra_id: Option<i64>,
}

/// Whoever a session belongs to: their account, and their 42 player if they
/// have one. The 42 pool only ever looks at `intra`.
#[derive(Debug, Clone)]
pub struct Viewer {
    pub account: Account,
    pub intra: Option<User>,
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

// ------------------------------------------------------------- accounts

/// The account that plays as 42 user `intra_id`, created on first sign-in.
/// Named after the login, unless a non-42 account took that name first, in
/// which case it gets `login-42`, `login-42-2`, and so on. Real 42 users (not
/// stand-ins, whose ids are negative) also get a `42` identity row.
pub async fn account_for_intra(db: &Db, intra_id: i64, login: &str) -> Result<Account> {
    let mut tx = db.begin().await?;
    let now = ts(Utc::now());

    let existing: Option<(i64, String)> =
        sqlx::query_as("SELECT id, username FROM accounts WHERE intra_id = ?1")
            .bind(intra_id)
            .fetch_optional(&mut *tx)
            .await?;
    let (id, username) = match existing {
        Some(found) => {
            sqlx::query("UPDATE accounts SET last_seen_at = ?1 WHERE id = ?2")
                .bind(&now)
                .bind(found.0)
                .execute(&mut *tx)
                .await?;
            found
        }
        None => {
            let username = free_username(&mut tx, login).await?;
            let (id,): (i64,) = sqlx::query_as(
                "INSERT INTO accounts (username, intra_id, created_at, last_seen_at)
                 VALUES (?1, ?2, ?3, ?3) RETURNING id",
            )
            .bind(&username)
            .bind(intra_id)
            .bind(&now)
            .fetch_one(&mut *tx)
            .await?;
            (id, username)
        }
    };

    if intra_id > 0 {
        sqlx::query(
            "INSERT INTO identities (provider, subject, account_id, created_at)
             VALUES ('42', ?1, ?2, ?3)
             ON CONFLICT(provider, subject) DO NOTHING",
        )
        .bind(intra_id.to_string())
        .bind(id)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Account {
        id,
        username,
        intra_id: Some(intra_id),
    })
}

/// `wanted` if no account has it yet, else the first free `wanted-42`,
/// `wanted-42-2`, ... Usernames compare case-insensitively.
async fn free_username(tx: &mut sqlx::SqliteConnection, wanted: &str) -> Result<String> {
    let mut candidate = wanted.to_string();
    for n in 1.. {
        let taken: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM accounts WHERE username = ?1")
            .bind(&candidate)
            .fetch_optional(&mut *tx)
            .await?;
        if taken.is_none() {
            break;
        }
        candidate = match n {
            1 => format!("{wanted}-42"),
            _ => format!("{wanted}-42-{n}"),
        };
    }
    Ok(candidate)
}

// ------------------------------------------------------------- sessions

pub async fn create_session(db: &Db, token: &str, account_id: i64, ttl_days: i64) -> Result<()> {
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO account_sessions (token, account_id, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4)",
    )
    .bind(token)
    .bind(account_id)
    .bind(ts(now))
    .bind(ts(now + chrono::Duration::days(ttl_days)))
    .execute(db)
    .await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct SessionRow {
    account_id: i64,
    username: String,
    intra_id: Option<i64>,
    login: Option<String>,
    display_name: Option<String>,
    image_url: Option<String>,
}

/// Who an unexpired session belongs to. A banned or deleted account counts as
/// signed out, so ending someone's access never depends on finding every
/// session they have.
pub async fn viewer_for_session(db: &Db, token: &str) -> Result<Option<Viewer>> {
    let row = sqlx::query_as::<_, SessionRow>(
        "SELECT a.id AS account_id, a.username, a.intra_id,
                u.login, u.display_name, u.image_url
         FROM account_sessions s
         JOIN accounts a ON a.id = s.account_id
         LEFT JOIN users u ON u.id = a.intra_id
         WHERE s.token = ?1 AND s.expires_at > ?2
           AND a.banned_at IS NULL AND a.deleted_at IS NULL",
    )
    .bind(token)
    .bind(ts(Utc::now()))
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| {
        let intra = match (r.intra_id, r.login, r.display_name) {
            (Some(id), Some(login), Some(display_name)) => Some(User {
                id,
                login,
                display_name,
                image_url: r.image_url,
            }),
            _ => None,
        };
        Viewer {
            account: Account {
                id: r.account_id,
                username: r.username,
                intra_id: r.intra_id,
            },
            intra,
        }
    }))
}

pub async fn delete_session(db: &Db, token: &str) -> Result<()> {
    sqlx::query("DELETE FROM account_sessions WHERE token = ?1")
        .bind(token)
        .execute(db)
        .await?;
    Ok(())
}

/// Drop sessions and OAuth states that have aged out.
pub async fn purge_expired(db: &Db) -> Result<()> {
    let now = ts(Utc::now());
    sqlx::query("DELETE FROM account_sessions WHERE expires_at <= ?1")
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
pub mod test_support {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT: AtomicU32 = AtomicU32::new(0);

    /// A throwaway on-disk database. `:memory:` would give every pooled
    /// connection its own empty database, so a real file it is.
    pub struct TempDb {
        pub path: std::path::PathBuf,
        pub db: Db,
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

    pub async fn temp_db() -> TempDb {
        let mut t = temp_db_path();
        t.db = connect(&format!("sqlite://{}", t.path.display()))
            .await
            .unwrap();
        t
    }

    /// A fresh path, cleaned up on drop, with nothing opened on it yet; `db`
    /// is a lazy placeholder until a test connects for real.
    pub fn temp_db_path() -> TempDb {
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
        let db = SqlitePoolOptions::new().connect_lazy("sqlite::memory:").unwrap();
        TempDb { path, db }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

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

    /// A signed-in 42 player: their `users` row, their account and a session.
    async fn signed_in(db: &Db, intra_id: i64, login: &str, token: &str) -> Account {
        upsert_user(db, intra_id, login, &format!("{login} name"), None)
            .await
            .unwrap();
        let account = account_for_intra(db, intra_id, login).await.unwrap();
        create_session(db, token, account.id, 30).await.unwrap();
        account
    }

    /// An account with no 42 player behind it, the kind other sign-in methods
    /// will create.
    async fn plain_account(db: &Db, username: &str) -> i64 {
        let (id,): (i64,) = sqlx::query_as(
            "INSERT INTO accounts (username, created_at, last_seen_at)
             VALUES (?1, '2026-10-01T00:00:00Z', '2026-10-01T00:00:00Z') RETURNING id",
        )
        .bind(username)
        .fetch_one(db)
        .await
        .unwrap();
        id
    }

    async fn count(db: &Db, sql: &str) -> i64 {
        let (n,): (i64,) = sqlx::query_as(sql).fetch_one(db).await.unwrap();
        n
    }

    #[tokio::test]
    async fn a_session_resolves_to_its_user_until_it_is_deleted() {
        let t = temp_db().await;
        upsert_user(&t.db, 7, "megardes", "Mehras", Some("https://cdn/x.jpg"))
            .await
            .unwrap();
        let account = account_for_intra(&t.db, 7, "megardes").await.unwrap();
        create_session(&t.db, "tok", account.id, 30).await.unwrap();

        let v = viewer_for_session(&t.db, "tok").await.unwrap().unwrap();
        assert_eq!(v.account, account);
        let u = v.intra.expect("a 42 account plays as its 42 user");
        assert_eq!((u.id, u.login.as_str()), (7, "megardes"));
        assert_eq!(u.image_url.as_deref(), Some("https://cdn/x.jpg"));

        delete_session(&t.db, "tok").await.unwrap();
        assert!(viewer_for_session(&t.db, "tok").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_expired_session_stops_resolving_and_gets_swept() {
        let t = temp_db().await;
        upsert_user(&t.db, 7, "megardes", "Mehras", None)
            .await
            .unwrap();
        let account = account_for_intra(&t.db, 7, "megardes").await.unwrap();
        create_session(&t.db, "stale", account.id, -1).await.unwrap(); // expired yesterday
        create_session(&t.db, "fresh", account.id, 30).await.unwrap();

        assert!(viewer_for_session(&t.db, "stale").await.unwrap().is_none());
        purge_expired(&t.db).await.unwrap();
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM account_sessions").await, 1);
        assert!(viewer_for_session(&t.db, "fresh").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn signing_in_again_reuses_the_same_account() {
        let t = temp_db().await;
        let first = signed_in(&t.db, 7, "megardes", "a").await;
        let again = signed_in(&t.db, 7, "megardes", "b").await;
        assert_eq!(first, again);
        assert_eq!(first.username, "megardes");
        assert_eq!(first.intra_id, Some(7));
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, 1);
        // One identity, however often they sign in.
        assert_eq!(
            count(&t.db, "SELECT COUNT(*) FROM identities WHERE provider = '42' AND subject = '7'").await,
            1
        );
        // Both sessions lead to the same person.
        for token in ["a", "b"] {
            let v = viewer_for_session(&t.db, token).await.unwrap().unwrap();
            assert_eq!(v.account.id, first.id);
        }
    }

    #[tokio::test]
    async fn a_stand_in_gets_an_account_but_no_42_identity() {
        let t = temp_db().await;
        let alice = create_test_user(&t.db, "alice").await.unwrap();
        let account = account_for_intra(&t.db, alice.id, "alice").await.unwrap();
        assert_eq!(account.intra_id, Some(alice.id));
        // A negative id is not anyone's intra account, so nothing claims it is.
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM identities").await, 0);
    }

    #[tokio::test]
    async fn a_42_login_taken_by_another_account_gets_a_suffix() {
        let t = temp_db().await;
        // Someone without 42 already goes by these names, in any case.
        plain_account(&t.db, "MeGardes").await;
        plain_account(&t.db, "megardes-42").await;

        let account = signed_in(&t.db, 7, "megardes", "tok").await;
        assert_eq!(account.username, "megardes-42-2");
        // The 42 pool still knows them by their login.
        let v = viewer_for_session(&t.db, "tok").await.unwrap().unwrap();
        assert_eq!(v.intra.unwrap().login, "megardes");
    }

    #[tokio::test]
    async fn an_account_without_42_has_no_player_in_the_42_pool() {
        let t = temp_db().await;
        let id = plain_account(&t.db, "stranger").await;
        create_session(&t.db, "tok", id, 30).await.unwrap();

        let v = viewer_for_session(&t.db, "tok").await.unwrap().unwrap();
        assert_eq!(v.account.username, "stranger");
        assert_eq!(v.account.intra_id, None);
        assert!(v.intra.is_none());
    }

    #[tokio::test]
    async fn a_banned_or_deleted_account_is_signed_out_everywhere() {
        let t = temp_db().await;
        let banned = signed_in(&t.db, 7, "megardes", "b1").await;
        create_session(&t.db, "b2", banned.id, 30).await.unwrap();
        let gone = signed_in(&t.db, 8, "other", "d1").await;

        sqlx::query("UPDATE accounts SET banned_at = '2026-10-01T00:00:00Z' WHERE id = ?1")
            .bind(banned.id)
            .execute(&t.db)
            .await
            .unwrap();
        sqlx::query("UPDATE accounts SET deleted_at = '2026-10-01T00:00:00Z' WHERE id = ?1")
            .bind(gone.id)
            .execute(&t.db)
            .await
            .unwrap();

        for token in ["b1", "b2", "d1"] {
            assert!(viewer_for_session(&t.db, token).await.unwrap().is_none(), "{token}");
        }
    }

    #[tokio::test]
    async fn removing_the_demo_takes_impersonated_bots_accounts_with_it() {
        let t = temp_db().await;
        let guesses = vec![DemoGuess { round_date: "2026-09-05".into(), bot: 0, value: 3 }];
        insert_demo(&t.db, &guesses).await.unwrap();
        let bot = user_by_login(&t.db, "bot-001").await.unwrap().unwrap();
        let account = account_for_intra(&t.db, bot.id, &bot.login).await.unwrap();
        create_session(&t.db, "tok", account.id, 30).await.unwrap();
        // A real player is left alone.
        signed_in(&t.db, 7, "megardes", "mine").await;

        remove_demo(&t.db).await.unwrap();
        assert!(viewer_for_session(&t.db, "tok").await.unwrap().is_none());
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, 1);
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM account_sessions").await, 1);
        assert!(viewer_for_session(&t.db, "mine").await.unwrap().is_some());
    }

    // ------------------------------------------------- the accounts migration

    /// A database as it was before accounts existed: migrations 0001-0004
    /// only, run from a copy of those files so the test does not depend on
    /// sqlx internals. `connect` then applies the rest, as a deploy would.
    async fn database_before_accounts() -> (TempDb, Db) {
        let t = temp_db_path();
        let dir = t.path.with_extension("migrations");
        std::fs::create_dir_all(&dir).unwrap();
        for entry in std::fs::read_dir("migrations").unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            if name.as_str() < "0005" {
                std::fs::copy(entry.path(), dir.join(&name)).unwrap();
            }
        }
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", t.path.display()))
            .unwrap()
            .create_if_missing(true)
            .foreign_keys(true);
        let old = SqlitePoolOptions::new().connect_with(opts).await.unwrap();
        sqlx::migrate::Migrator::new(dir.as_path())
            .await
            .unwrap()
            .run(&old)
            .await
            .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        (t, old)
    }

    #[tokio::test]
    async fn the_accounts_migration_keeps_everyone_signed_in_and_the_42_pool_unchanged() {
        let (mut t, old) = database_before_accounts().await;
        let exec = |sql: &'static str| {
            let old = old.clone();
            async move { sqlx::query(sql).execute(&old).await.unwrap() }
        };
        exec("INSERT INTO users (id, login, display_name, image_url, created_at, last_seen_at) VALUES
              (11, 'megardes', 'Mehras', 'https://cdn/m.jpg', '2026-09-01T10:00:00Z', '2026-10-01T10:00:00Z'),
              (12, 'other',    'Other',  NULL,                '2026-09-02T10:00:00Z', '2026-10-02T10:00:00Z'),
              (13, 'quiet',    'Quiet',  NULL,                '2026-09-03T10:00:00Z', '2026-09-03T10:00:00Z'),
              (-1, 'alice',    'alice (test)', NULL,          '2026-09-04T10:00:00Z', '2026-09-04T10:00:00Z'),
              (-2, 'bot-001',  'Bot 001', NULL,               '2026-09-05T10:00:00Z', '2026-09-05T10:00:00Z'),
              (-3, 'bot-002',  'Bot 002', NULL,               '2026-09-05T10:00:00Z', '2026-09-05T10:00:00Z')").await;
        exec("INSERT INTO sessions (token, user_id, created_at, expires_at) VALUES
              ('m1', 11, '2026-10-01T10:00:00Z', '2099-01-01T00:00:00Z'),
              ('m2', 11, '2026-10-02T10:00:00Z', '2099-01-01T00:00:00Z'),
              ('o1', 12, '2026-10-01T10:00:00Z', '2099-01-01T00:00:00Z'),
              ('old', 12, '2026-01-01T10:00:00Z', '2026-02-01T00:00:00Z'),
              ('a1', -1, '2026-10-01T10:00:00Z', '2099-01-01T00:00:00Z'),
              ('b1', -2, '2026-10-01T10:00:00Z', '2099-01-01T00:00:00Z')").await;
        exec("INSERT INTO guesses (round_date, user_id, value, submitted_at, participates) VALUES
              ('2026-09-05', 11, 1, 'x', 1), ('2026-09-05', 12, 1, 'x', 1), ('2026-09-05', 13, 2, 'x', 1),
              ('2026-09-05', -1, 1, 'x', 0),
              ('2026-09-06', 11, 4, 'x', 1), ('2026-09-06', -2, 3, 'x', 1), ('2026-09-06', -3, 3, 'x', 1),
              ('2026-09-07', 12, 9, 'x', 1)").await;
        exec("INSERT INTO payouts (round_date, user_id, outcome, message, paid_at, participants)
              VALUES ('2026-09-05', 13, 'given', 'ok', 'x', 3)").await;
        exec("INSERT INTO oauth_states (state, created_at, expires_at)
              VALUES ('pending', '2026-10-01T10:00:00Z', '2099-01-01T00:00:00Z')").await;

        // What the 42 pool says before the migration...
        let open = "2026-09-07";
        let before_rounds = closed_rounds(&old, open, 100).await.unwrap();
        let before_board = leaderboard(&old, open, 100).await.unwrap();
        let before_tallies = range_tallies(&old, "", open).await.unwrap();
        old.close().await;

        t.db = connect(&format!("sqlite://{}", t.path.display())).await.unwrap();
        let db = &t.db;

        // ...is exactly what it says after.
        let fmt = |v: &dyn std::fmt::Debug| format!("{v:?}");
        assert_eq!(fmt(&closed_rounds(db, open, 100).await.unwrap()), fmt(&before_rounds));
        assert_eq!(fmt(&leaderboard(db, open, 100).await.unwrap()), fmt(&before_board));
        assert_eq!(range_tallies(db, "", open).await.unwrap(), before_tallies);
        assert_eq!(unpaid_round(db, open, "2026-09-05").await.unwrap(), None, "still settled");

        // Every live session still signs in the same person, as their 42 user.
        for (token, intra_id, login) in [
            ("m1", 11, "megardes"),
            ("m2", 11, "megardes"),
            ("o1", 12, "other"),
            ("a1", -1, "alice"),
            ("b1", -2, "bot-001"),
        ] {
            let v = viewer_for_session(db, token).await.unwrap().unwrap_or_else(|| panic!("{token}"));
            assert_eq!(v.account.intra_id, Some(intra_id), "{token}");
            assert_eq!(v.account.username, login, "{token}");
            assert_eq!(v.intra.unwrap().login, login, "{token}");
        }
        // Both of one person's sessions point at one account.
        let m1 = viewer_for_session(db, "m1").await.unwrap().unwrap();
        let m2 = viewer_for_session(db, "m2").await.unwrap().unwrap();
        assert_eq!(m1.account.id, m2.account.id);
        assert_eq!(m1.intra.unwrap().image_url.as_deref(), Some("https://cdn/m.jpg"));
        // An expired one came across expired, and the sweep still removes it.
        assert!(viewer_for_session(db, "old").await.unwrap().is_none());
        purge_expired(db).await.unwrap();
        assert_eq!(count(db, "SELECT COUNT(*) FROM account_sessions").await, 5);

        // Accounts: every person and the hand-made stand-in, plus the one bot
        // somebody was signed in as. The idle bot gets none.
        let mut names: Vec<(String, Option<i64>)> =
            sqlx::query_as("SELECT username, intra_id FROM accounts ORDER BY username")
                .fetch_all(db)
                .await
                .unwrap();
        names.sort();
        assert_eq!(
            names,
            [
                ("alice".to_string(), Some(-1)),
                ("bot-001".to_string(), Some(-2)),
                ("megardes".to_string(), Some(11)),
                ("other".to_string(), Some(12)),
                ("quiet".to_string(), Some(13)),
            ]
        );
        // Dates come from the users rows, not from the day of the deploy.
        let (created, seen): (String, String) =
            sqlx::query_as("SELECT created_at, last_seen_at FROM accounts WHERE intra_id = 11")
                .fetch_one(db)
                .await
                .unwrap();
        assert_eq!((created.as_str(), seen.as_str()), ("2026-09-01T10:00:00Z", "2026-10-01T10:00:00Z"));

        // A 42 identity for the three real students only.
        let ids: Vec<(String,)> =
            sqlx::query_as("SELECT subject FROM identities WHERE provider = '42' ORDER BY subject")
                .fetch_all(db)
                .await
                .unwrap();
        assert_eq!(ids, [("11".into(),), ("12".into(),), ("13".into(),)]);

        // The old table is gone, and a sign-in started before the deploy can
        // still finish.
        assert_eq!(
            count(db, "SELECT COUNT(*) FROM sqlite_master WHERE name = 'sessions'").await,
            0
        );
        let (provider,): (String,) =
            sqlx::query_as("SELECT provider FROM oauth_states WHERE state = 'pending'")
                .fetch_one(db)
                .await
                .unwrap();
        assert_eq!(provider, "42");
        assert!(consume_oauth_state(db, "pending").await.unwrap());

        // The next sign-in of a migrated player finds their account rather
        // than making a second one.
        let again = account_for_intra(db, 13, "quiet").await.unwrap();
        assert_eq!(again.username, "quiet");
        assert_eq!(count(db, "SELECT COUNT(*) FROM accounts").await, 5);
    }

    /// Runs the migration against a copy of a real database, e.g. a backup
    /// of the live one, and checks every session survives and the 42 pool
    /// reads the same. The original is never touched.
    ///
    /// MIGRATE_CHECK_DB=/path/to/game.db cargo test migrating_a_copy -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "needs MIGRATE_CHECK_DB pointing at a real database"]
    async fn migrating_a_copy_of_a_real_database() {
        let source = std::env::var("MIGRATE_CHECK_DB").expect("set MIGRATE_CHECK_DB");
        let t = temp_db_path();
        // VACUUM INTO makes a consistent copy even of a database in WAL mode
        // with a live writer, and leaves the source alone.
        let src = SqliteConnectOptions::from_str(&format!("sqlite://{source}"))
            .unwrap()
            .read_only(true);
        let src = SqlitePoolOptions::new().max_connections(1).connect_with(src).await.unwrap();
        sqlx::query("VACUUM INTO ?1")
            .bind(t.path.to_str().unwrap())
            .execute(&src)
            .await
            .unwrap();
        src.close().await;

        let copy = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::from_str(&format!("sqlite://{}", t.path.display())).unwrap(),
            )
            .await
            .unwrap();
        let already: i64 = count(
            &copy,
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'accounts'",
        )
        .await;
        assert_eq!(already, 0, "that database is already past the accounts migration");
        let version = count(&copy, "SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations").await;
        assert_eq!(
            version, 4,
            "this check compares a database at migration 0004 (what v1.6 runs) with \
             the same one after 0005; that one is at {version}"
        );

        let now = ts(Utc::now());
        let sessions: Vec<(String, i64)> = sqlx::query_as(
            "SELECT token, user_id FROM sessions WHERE expires_at > ?1 ORDER BY token",
        )
        .bind(&now)
        .fetch_all(&copy)
        .await
        .unwrap();
        let users = count(&copy, "SELECT COUNT(*) FROM users").await;
        let guesses = count(&copy, "SELECT COUNT(*) FROM guesses").await;
        let payouts = count(&copy, "SELECT COUNT(*) FROM payouts").await;
        let open = crate::round::Round::current(Utc::now()).key();
        let before_rounds = format!("{:?}", closed_rounds(&copy, &open, i64::MAX).await.unwrap());
        let before_board = format!("{:?}", leaderboard(&copy, &open, i64::MAX).await.unwrap());
        let before_tallies = range_tallies(&copy, "", &open).await.unwrap();
        copy.close().await;

        let started = std::time::Instant::now();
        let db = connect(&format!("sqlite://{}", t.path.display())).await.unwrap();
        let took = started.elapsed();

        for (token, user_id) in &sessions {
            let v = viewer_for_session(&db, token)
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("a live session for user {user_id} was lost"));
            assert_eq!(v.account.intra_id, Some(*user_id));
            assert_eq!(v.intra.map(|u| u.id), Some(*user_id));
        }
        assert_eq!(count(&db, "SELECT COUNT(*) FROM users").await, users);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM guesses").await, guesses);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM payouts").await, payouts);
        assert_eq!(format!("{:?}", closed_rounds(&db, &open, i64::MAX).await.unwrap()), before_rounds);
        assert_eq!(format!("{:?}", leaderboard(&db, &open, i64::MAX).await.unwrap()), before_board);
        assert_eq!(range_tallies(&db, "", &open).await.unwrap(), before_tallies);
        // No real student is left without an account.
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM users u WHERE u.id > 0
                        AND NOT EXISTS (SELECT 1 FROM accounts a WHERE a.intra_id = u.id)").await,
            0
        );
        println!(
            "migrated in {took:?}: {users} users, {} accounts, {} live sessions kept, \
             {guesses} guesses and {payouts} payouts unchanged",
            count(&db, "SELECT COUNT(*) FROM accounts").await,
            sessions.len(),
        );
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
