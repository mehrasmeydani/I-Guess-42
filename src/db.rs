use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;

pub type Db = SqlitePool;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct User {
    pub id: i64,
    pub login: String,
    pub display_name: String,
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
    pub winner_image: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LeaderboardRow {
    pub login: String,
    pub display_name: String,
    pub image_url: Option<String>,
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

pub async fn upsert_guess(db: &Db, round_date: &str, user_id: i64, value: i64) -> Result<()> {
    sqlx::query(
        "INSERT INTO guesses (round_date, user_id, value, submitted_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(round_date, user_id) DO UPDATE SET
             value = excluded.value,
             submitted_at = excluded.submitted_at",
    )
    .bind(round_date)
    .bind(user_id)
    .bind(value)
    .bind(ts(Utc::now()))
    .execute(db)
    .await?;
    Ok(())
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
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM guesses WHERE round_date = ?1")
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
        FROM guesses WHERE round_date < ?1
        GROUP BY round_date
    ),
    uniq AS (
        SELECT round_date, value
        FROM guesses WHERE round_date < ?1
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
                u.display_name AS winner_name,
                u.image_url    AS winner_image
         FROM rounds r
         LEFT JOIN winners w ON w.round_date = r.round_date
         LEFT JOIN guesses g ON g.round_date = w.round_date AND g.value = w.value
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
                u.display_name AS winner_name,
                u.image_url    AS winner_image
         FROM rounds r
         LEFT JOIN winners w ON w.round_date = r.round_date
         LEFT JOIN guesses g ON g.round_date = w.round_date AND g.value = w.value
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

pub async fn leaderboard(db: &Db, open_round: &str, limit: i64) -> Result<Vec<LeaderboardRow>> {
    let sql = format!(
        "{WINNER_CTE}
         SELECT u.login, u.display_name, u.image_url, COUNT(*) AS wins
         FROM winners w
         JOIN guesses g ON g.round_date = w.round_date AND g.value = w.value
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
            upsert_guess(db, round, *user_id, *value).await.unwrap();
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
    async fn resubmitting_replaces_the_previous_guess() {
        let t = temp_db().await;
        seed(&t.db, "2026-09-05", &[(1, 50)]).await;
        upsert_guess(&t.db, "2026-09-05", 1, 3).await.unwrap();

        assert_eq!(my_guess(&t.db, "2026-09-05", 1).await.unwrap(), Some(3));
        assert_eq!(guess_count(&t.db, "2026-09-05").await.unwrap(), 1);
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
