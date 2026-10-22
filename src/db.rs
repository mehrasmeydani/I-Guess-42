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
    /// Proved it can be reached; only verified accounts play the global pool.
    pub verified: bool,
}

/// Whoever a session belongs to: their account, and their 42 player if they
/// have one. The 42 pool only ever looks at `intra`.
#[derive(Debug, Clone)]
pub struct Viewer {
    pub account: Account,
    pub intra: Option<User>,
    /// Their points in the global round (economy.rs).
    pub points: i64,
}

/// Which game a query is about. The two pools are the same game on separate
/// tables of the same shape: `guesses` keyed by intra id with `users` for
/// names, and `global_guesses` keyed by account id with `global_players`.
/// Table names only ever come from here, never from input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pool {
    /// The 42 Vienna students' round, as it has always been.
    Vienna,
    /// Open to every verified account.
    Global,
}

impl Pool {
    fn guesses(self) -> &'static str {
        match self {
            Pool::Vienna => "guesses",
            Pool::Global => "global_guesses",
        }
    }

    fn players(self) -> &'static str {
        match self {
            Pool::Vienna => "users",
            Pool::Global => "global_players",
        }
    }

    pub fn is_global(self) -> bool {
        self == Pool::Global
    }

    /// The path prefix of this pool's pages: `/results` or `/g/results`.
    pub fn base(self) -> &'static str {
        match self {
            Pool::Vienna => "",
            Pool::Global => "/g",
        }
    }
}

/// One closed round, with its winner if the round produced one.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RoundSummary {
    pub round_date: String,
    pub total: i64,
    pub winning_value: Option<i64>,
    /// The winner's id in this pool (intra id or account id), for an
    /// anonymous label when their name may not be shown.
    pub winner_id: Option<i64>,
    pub winner_login: Option<String>,
    pub winner_name: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LeaderboardRow {
    /// The player's id in this pool, as in `RoundSummary::winner_id`.
    pub id: i64,
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

/// A transaction that will write, holding the write lock from its first
/// statement (`BEGIN IMMEDIATE`). A plain `BEGIN` reads first and upgrades
/// later, and an upgrade that finds another writer has committed in the
/// meantime fails at once with "database is locked" instead of waiting out
/// the busy timeout - which the 12:42 rush of guesses, each reading a
/// balance before writing a stake, would hit. Every transaction in this file
/// writes, so every one starts here.
async fn write_tx(db: &Db) -> Result<sqlx::Transaction<'static, sqlx::Sqlite>> {
    Ok(db.begin_with("BEGIN IMMEDIATE").await?)
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
    let mut tx = write_tx(db).await?;
    let now = ts(Utc::now());

    // The account playing as this 42 user, or else one that already signs in
    // with this 42 login without playing the 42 pool (made while the student's
    // campus was not allowed in), which now starts to.
    let existing: Option<(i64, String)> = sqlx::query_as(
        "SELECT id, username FROM accounts WHERE intra_id = ?1
         UNION ALL
         SELECT a.id, a.username FROM identities i JOIN accounts a ON a.id = i.account_id
         WHERE i.provider = '42' AND i.subject = CAST(?1 AS TEXT) AND a.intra_id IS NULL
         LIMIT 1",
    )
    .bind(intra_id)
    .fetch_optional(&mut *tx)
    .await?;
    let (id, username) = match existing {
        Some(found) => {
            sqlx::query(
                "UPDATE accounts SET last_seen_at = ?1, verified_at = COALESCE(verified_at, ?1),
                                     intra_id = ?3
                 WHERE id = ?2",
            )
                .bind(&now)
                .bind(found.0)
                .bind(intra_id)
                .execute(&mut *tx)
                .await?;
            found
        }
        None => {
            let username = free_username(&mut tx, login).await?;
            // 42 vouches for its students, so the account is verified at once.
            let (id,): (i64,) = sqlx::query_as(
                "INSERT INTO accounts (username, intra_id, created_at, last_seen_at, verified_at)
                 VALUES (?1, ?2, ?3, ?3, ?3) RETURNING id",
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
        verified: true,
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

// --------------------------------------------------------------- sign-in

/// True if nobody goes by `name` yet: no account has it, and it is not the
/// login of any 42 player, so a stranger cannot take a student's name before
/// they first come to play the global pool.
pub async fn username_available(db: &Db, name: &str) -> Result<bool> {
    let (taken,): (i64,) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM accounts WHERE username = ?1)
              + (SELECT COUNT(*) FROM users WHERE login = ?1 COLLATE NOCASE)",
    )
    .bind(name)
    .fetch_one(db)
    .await?;
    Ok(taken == 0)
}

/// What came of trying to create an account.
#[derive(Debug)]
pub enum NewAccount {
    Created(Account),
    /// That email already signs in somewhere. The caller must not say so to
    /// the visitor; see `mail::exists`.
    EmailTaken,
    UsernameTaken,
}

/// A new account that signs in with `email` and a password, not yet
/// verified. The caller has checked the username's shape and the password.
pub async fn create_password_account(
    db: &Db,
    email: &str,
    username: &str,
    hash: &str,
    ip_hash: Option<&str>,
) -> Result<NewAccount> {
    let mut tx = write_tx(db).await?;
    let now = ts(Utc::now());

    // Any account with this address, however it signs in: one person, one
    // account. The owner is told by mail rather than the visitor.
    let (email_taken,): (i64,) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM passwords WHERE email = ?1)
              + (SELECT COUNT(*) FROM identities WHERE email = ?1 COLLATE NOCASE)",
    )
    .bind(email)
    .fetch_one(&mut *tx)
    .await?;
    if email_taken > 0 {
        return Ok(NewAccount::EmailTaken);
    }
    let (name_taken,): (i64,) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM accounts WHERE username = ?1)
              + (SELECT COUNT(*) FROM users WHERE login = ?1 COLLATE NOCASE)",
    )
    .bind(username)
    .fetch_one(&mut *tx)
    .await?;
    if name_taken > 0 {
        return Ok(NewAccount::UsernameTaken);
    }

    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO accounts (username, created_at, last_seen_at, signup_ip_hash)
         VALUES (?1, ?2, ?2, ?3) RETURNING id",
    )
    .bind(username)
    .bind(&now)
    .bind(ip_hash)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO identities (provider, subject, account_id, email, created_at)
         VALUES ('email', ?1, ?2, ?1, ?3)",
    )
    .bind(email)
    .bind(id)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO passwords (account_id, email, hash, created_at, changed_at)
         VALUES (?1, ?2, ?3, ?4, ?4)",
    )
    .bind(id)
    .bind(email)
    .bind(hash)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(NewAccount::Created(Account {
        id,
        username: username.to_string(),
        intra_id: None,
        verified: false,
    }))
}

/// The account and password hash behind an email, if it may sign in. Banned
/// and deleted accounts are not found, so they get the same answer as a
/// wrong password.
pub async fn password_login(db: &Db, email: &str) -> Result<Option<(i64, String)>> {
    let row = sqlx::query_as(
        "SELECT p.account_id, p.hash FROM passwords p
         JOIN accounts a ON a.id = p.account_id
         WHERE p.email = ?1 AND a.banned_at IS NULL AND a.deleted_at IS NULL",
    )
    .bind(email)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

/// As [`password_login`], found by email if `typed` contains an `@` (no
/// username can) and by username otherwise. Also gives the email, so a
/// caller can count attempts per account rather than per spelling.
pub async fn password_login_any(db: &Db, typed: &str) -> Result<Option<(i64, String, String)>> {
    let by = if typed.contains('@') { "p.email" } else { "a.username" };
    let sql = format!(
        "SELECT p.account_id, p.hash, p.email FROM passwords p
         JOIN accounts a ON a.id = p.account_id
         WHERE {by} = ?1 AND a.banned_at IS NULL AND a.deleted_at IS NULL"
    );
    let row = sqlx::query_as(&sql).bind(typed).fetch_optional(db).await?;
    Ok(row)
}

/// The email an account signs in with by password, and whether it is verified.
pub async fn password_email(db: &Db, account_id: i64) -> Result<Option<(String, bool)>> {
    let row = sqlx::query_as(
        "SELECT email, verified_at IS NOT NULL FROM passwords WHERE account_id = ?1",
    )
    .bind(account_id)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

/// Remembers the hash of an emailed link's token.
pub async fn store_link_token(
    db: &Db,
    account_id: i64,
    purpose: &str,
    token_hash: &str,
    lifetime: chrono::Duration,
) -> Result<()> {
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO email_tokens (token_hash, account_id, purpose, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(token_hash)
    .bind(account_id)
    .bind(purpose)
    .bind(ts(now))
    .bind(ts(now + lifetime))
    .execute(db)
    .await?;
    Ok(())
}

/// The account a link is for, if it is unused, unexpired and for `purpose`,
/// without using it up. For showing the form a reset link leads to.
pub async fn peek_link_token(db: &Db, purpose: &str, token_hash: &str) -> Result<Option<i64>> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT account_id FROM email_tokens
         WHERE token_hash = ?1 AND purpose = ?2 AND used_at IS NULL AND expires_at > ?3",
    )
    .bind(token_hash)
    .bind(purpose)
    .bind(ts(Utc::now()))
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| r.0))
}

/// Uses a link up and says whose it was. One statement, so the same link
/// opened twice at once still works only once.
pub async fn use_link_token(db: &Db, purpose: &str, token_hash: &str) -> Result<Option<i64>> {
    let now = ts(Utc::now());
    let row: Option<(i64,)> = sqlx::query_as(
        "UPDATE email_tokens SET used_at = ?3
         WHERE token_hash = ?1 AND purpose = ?2 AND used_at IS NULL AND expires_at > ?3
         RETURNING account_id",
    )
    .bind(token_hash)
    .bind(purpose)
    .bind(&now)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| r.0))
}

/// The account's email is confirmed, and with it the account.
pub async fn mark_email_verified(db: &Db, account_id: i64) -> Result<()> {
    let now = ts(Utc::now());
    let mut tx = write_tx(db).await?;
    sqlx::query("UPDATE passwords SET verified_at = COALESCE(verified_at, ?1) WHERE account_id = ?2")
        .bind(&now)
        .bind(account_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE accounts SET verified_at = COALESCE(verified_at, ?1) WHERE id = ?2")
        .bind(&now)
        .bind(account_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// A new password, and every session of the account ended: whoever knew the
/// old one is signed out with it.
pub async fn set_password(db: &Db, account_id: i64, hash: &str) -> Result<()> {
    let mut tx = write_tx(db).await?;
    sqlx::query("UPDATE passwords SET hash = ?1, changed_at = ?2 WHERE account_id = ?3")
        .bind(hash)
        .bind(ts(Utc::now()))
        .bind(account_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM account_sessions WHERE account_id = ?1")
        .bind(account_id)
        .execute(&mut *tx)
        .await?;
    // Any other reset link still lying in an inbox stops working too.
    sqlx::query("UPDATE email_tokens SET used_at = ?1 WHERE account_id = ?2 AND purpose = 'reset' AND used_at IS NULL")
        .bind(ts(Utc::now()))
        .bind(account_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn account_verified(db: &Db, account_id: i64) -> Result<bool> {
    let row: Option<(bool,)> = sqlx::query_as("SELECT verified_at IS NOT NULL FROM accounts WHERE id = ?1")
        .bind(account_id)
        .fetch_optional(db)
        .await?;
    Ok(row.is_some_and(|r| r.0))
}

pub async fn account_username(db: &Db, account_id: i64) -> Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT username FROM accounts WHERE id = ?1")
        .bind(account_id)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|r| r.0))
}

pub async fn mails_sent_since(db: &Db, since: &str) -> Result<i64> {
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM mail_log WHERE sent_at >= ?1")
        .bind(since)
        .fetch_one(db)
        .await?;
    Ok(n)
}

pub async fn log_mail(db: &Db, purpose: &str) -> Result<()> {
    sqlx::query("INSERT INTO mail_log (purpose, sent_at) VALUES (?1, ?2)")
        .bind(purpose)
        .bind(ts(Utc::now()))
        .execute(db)
        .await?;
    Ok(())
}

// --------------------------------------------------------------- account

/// One way an account signs in, as the account page lists it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Login {
    pub provider: String,
    pub subject: String,
    pub email: Option<String>,
}

pub async fn account_logins(db: &Db, account_id: i64) -> Result<Vec<Login>> {
    let rows = sqlx::query_as::<_, Login>(
        "SELECT provider, subject, email FROM identities WHERE account_id = ?1
         ORDER BY CASE provider WHEN '42' THEN 0 WHEN 'github' THEN 1 WHEN 'google' THEN 2 ELSE 3 END",
    )
    .bind(account_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// When the username last changed, if it ever did.
pub async fn username_changed_at(db: &Db, account_id: i64) -> Result<Option<String>> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT username_changed_at FROM accounts WHERE id = ?1")
            .bind(account_id)
            .fetch_optional(db)
            .await?;
    Ok(row.and_then(|r| r.0))
}

/// Renames an account; false if the name was taken in the meantime.
/// `counts`: whether it starts the once-a-month wait. Picking a name right
/// after signing up does not.
pub async fn rename_account(db: &Db, account_id: i64, username: &str, counts: bool) -> Result<bool> {
    let mut tx = write_tx(db).await?;
    let (taken,): (i64,) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM accounts WHERE username = ?1 AND id != ?2)
              + (SELECT COUNT(*) FROM users u WHERE u.login = ?1 COLLATE NOCASE
                 AND NOT EXISTS (SELECT 1 FROM accounts a WHERE a.id = ?2 AND a.intra_id = u.id))",
    )
    .bind(username)
    .bind(account_id)
    .fetch_one(&mut *tx)
    .await?;
    if taken > 0 {
        return Ok(false);
    }
    sqlx::query(
        "UPDATE accounts SET username = ?1,
                username_changed_at = CASE WHEN ?4 THEN ?2 ELSE username_changed_at END
         WHERE id = ?3",
    )
        .bind(username)
        .bind(ts(Utc::now()))
        .bind(account_id)
        .bind(counts)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

/// What came of removing a login.
#[derive(Debug, PartialEq, Eq)]
pub enum Unlinked {
    Removed,
    /// It is the only way left into the account.
    LastOne,
    /// The 42 login of an account that plays the 42 pool, which depends on it.
    Plays42,
    NotLinked,
}

pub async fn unlink_identity(db: &Db, account_id: i64, provider: &str) -> Result<Unlinked> {
    let mut tx = write_tx(db).await?;
    let (mine, total, plays_42): (i64, i64, bool) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM identities WHERE account_id = ?1 AND provider = ?2),
                (SELECT COUNT(*) FROM identities WHERE account_id = ?1),
                (SELECT intra_id IS NOT NULL FROM accounts WHERE id = ?1)",
    )
    .bind(account_id)
    .bind(provider)
    .fetch_one(&mut *tx)
    .await?;
    if mine == 0 {
        return Ok(Unlinked::NotLinked);
    }
    if provider == "42" && plays_42 {
        return Ok(Unlinked::Plays42);
    }
    if total <= mine {
        return Ok(Unlinked::LastOne);
    }
    sqlx::query("DELETE FROM identities WHERE account_id = ?1 AND provider = ?2")
        .bind(account_id)
        .bind(provider)
        .execute(&mut *tx)
        .await?;
    if provider == "email" {
        sqlx::query("DELETE FROM passwords WHERE account_id = ?1")
            .bind(account_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(Unlinked::Removed)
}

/// Gives an account that came in through a provider an email and password
/// too. False if that email already signs in somewhere.
pub async fn add_password_login(db: &Db, account_id: i64, email: &str, hash: &str) -> Result<bool> {
    let mut tx = write_tx(db).await?;
    let (taken,): (i64,) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM passwords WHERE email = ?1 OR account_id = ?2)
              + (SELECT COUNT(*) FROM identities WHERE provider = 'email' AND subject = ?1)
              + (SELECT COUNT(*) FROM identities WHERE email = ?1 COLLATE NOCASE AND account_id != ?2)",
    )
    .bind(email)
    .bind(account_id)
    .fetch_one(&mut *tx)
    .await?;
    if taken > 0 {
        return Ok(false);
    }
    let now = ts(Utc::now());
    sqlx::query(
        "INSERT INTO identities (provider, subject, account_id, email, created_at)
         VALUES ('email', ?1, ?2, ?1, ?3)",
    )
    .bind(email)
    .bind(account_id)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO passwords (account_id, email, hash, created_at, changed_at)
         VALUES (?1, ?2, ?3, ?4, ?4)",
    )
    .bind(account_id)
    .bind(email)
    .bind(hash)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Deletes an account as far as it can be deleted. Every way of signing in
/// to it goes, with its sessions and links, its network hashes and any
/// warning or ban reason, and its name becomes `deleted-<id>`. The row
/// itself stays, with its guesses, so the rounds it played in still add up;
/// they just no longer say who. A 42 student's `users` row stays too, under
/// its 42 id, which the 42 round's guesses and coalition payouts hang on,
/// but loses its login, name and picture: the 42 round's past results say
/// `deleted-<id>` as well. Signing in with 42 again fills them back in.
pub async fn delete_account(db: &Db, account_id: i64) -> Result<()> {
    let mut tx = write_tx(db).await?;
    sqlx::query(
        "UPDATE users SET login = 'deleted-' || ?1, display_name = 'deleted-' || ?1, image_url = NULL
         WHERE id = (SELECT intra_id FROM accounts WHERE id = ?1)",
    )
    .bind(account_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE global_guesses SET ip_hash = NULL WHERE user_id = ?1")
        .bind(account_id)
        .execute(&mut *tx)
        .await?;
    for sql in [
        "DELETE FROM identities WHERE account_id = ?1",
        "DELETE FROM passwords WHERE account_id = ?1",
        "DELETE FROM account_sessions WHERE account_id = ?1",
        "DELETE FROM email_tokens WHERE account_id = ?1",
        "DELETE FROM oauth_states WHERE link_account_id = ?1",
    ] {
        sqlx::query(sql).bind(account_id).execute(&mut *tx).await?;
    }
    sqlx::query(
        "UPDATE accounts SET username = 'deleted-' || id, intra_id = NULL, deleted_at = ?1,
                             signup_ip_hash = NULL, warn_reason = NULL, ban_reason = NULL
         WHERE id = ?2",
    )
    .bind(ts(Utc::now()))
    .bind(account_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

// ------------------------------------------------------------ moderation

/// An account as a moderator sees it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ModAccount {
    pub id: i64,
    pub username: String,
    /// The 42 login, for an account that plays the 42 pool.
    pub login: Option<String>,
    pub created_at: String,
    pub banned_at: Option<String>,
    pub ban_reason: Option<String>,
}

const MOD_ACCOUNT: &str = "SELECT a.id, a.username, u.login, a.created_at, a.banned_at, a.ban_reason
     FROM accounts a LEFT JOIN users u ON u.id = a.intra_id";

pub async fn mod_account(db: &Db, username: &str) -> Result<Option<ModAccount>> {
    let sql = format!("{MOD_ACCOUNT} WHERE a.username = ?1 AND a.deleted_at IS NULL");
    let row = sqlx::query_as::<_, ModAccount>(&sql)
        .bind(username)
        .fetch_optional(db)
        .await?;
    Ok(row)
}

pub async fn banned_accounts(db: &Db) -> Result<Vec<ModAccount>> {
    let sql = format!("{MOD_ACCOUNT} WHERE a.banned_at IS NOT NULL ORDER BY a.banned_at DESC");
    let rows = sqlx::query_as::<_, ModAccount>(&sql).fetch_all(db).await?;
    Ok(rows)
}

/// Bans an account: signed out everywhere and unable to sign in again, and
/// its guess in the open global round no longer counts (its stake refunded). Rounds already
/// closed are left as they were.
pub async fn ban_account(db: &Db, account_id: i64, reason: &str, open_round: &str) -> Result<()> {
    let mut tx = write_tx(db).await?;
    sqlx::query("UPDATE accounts SET banned_at = ?1, ban_reason = ?2 WHERE id = ?3")
        .bind(ts(Utc::now()))
        .bind(reason)
        .bind(account_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM account_sessions WHERE account_id = ?1")
        .bind(account_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE global_guesses SET participates = 0 WHERE user_id = ?1 AND round_date = ?2")
        .bind(account_id)
        .bind(open_round)
        .execute(&mut *tx)
        .await?;
    // Its bets on the open round are called off and refunded.
    void_bets(&mut tx, Some(account_id), open_round, "banned").await?;
    // Its stake comes back out of the pot: a voided guess pays for nothing.
    sqlx::query(
        "INSERT INTO ledger (account_id, round_date, kind, amount, ref, note, created_at)
         SELECT account_id, round_date, 'refund', -amount, 'stake:' || round_date, 'banned', ?3
         FROM ledger WHERE account_id = ?1 AND round_date = ?2 AND kind = 'stake'
         ON CONFLICT(account_id, kind, ref) DO NOTHING",
    )
    .bind(account_id)
    .bind(open_round)
    .bind(ts(Utc::now()))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn unban_account(db: &Db, account_id: i64) -> Result<()> {
    sqlx::query("UPDATE accounts SET banned_at = NULL, ban_reason = NULL WHERE id = ?1")
        .bind(account_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Accounts that look like they might be one person: a group of usernames
/// and what they have in common. For a moderator to look at, never acted on
/// by itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cluster {
    pub why: String,
    pub usernames: Vec<String>,
    /// A pattern that bends a round's result, not only a shared network:
    /// listed first, and marked.
    pub serious: bool,
}

/// Records a moderator's warning on an account, and gives back the address
/// to mail it to: its password email, else one a provider shared. None for
/// no such account.
pub async fn warn_account(db: &Db, account_id: i64, reason: &str) -> Result<Option<String>> {
    sqlx::query("UPDATE accounts SET warned_at = ?1, warn_reason = ?2 WHERE id = ?3")
        .bind(ts(Utc::now()))
        .bind(reason)
        .bind(account_id)
        .execute(db)
        .await?;
    let email: Option<String> = sqlx::query_scalar(
        "SELECT COALESCE(
             (SELECT email FROM passwords WHERE account_id = ?1),
             (SELECT email FROM identities WHERE account_id = ?1 AND email IS NOT NULL ORDER BY created_at LIMIT 1))",
    )
    .bind(account_id)
    .fetch_one(db)
    .await?;
    Ok(email)
}

/// Usernames of accounts a moderator has warned, for the flags to show.
pub async fn warned_usernames(db: &Db) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar("SELECT username FROM accounts WHERE warned_at IS NOT NULL AND banned_at IS NULL")
        .fetch_all(db)
        .await?)
}

/// Groups of at least `min` accounts that signed up from one address in the
/// last 30 days, or guessed in the global pool from one address in the last
/// 7, and runs of `min` or more sign-ups within ten minutes of each other.
/// The 42 pool is left out: every account there is a different student.
pub async fn clusters(db: &Db, min: i64) -> Result<Vec<Cluster>> {
    let now = Utc::now();
    let month_ago = ts(now - chrono::Duration::days(30));
    let week_ago = (now - chrono::Duration::days(7)).format("%Y-%m-%d").to_string();
    let split = |names: String| -> Vec<String> {
        let mut v: Vec<String> = names.split(',').map(str::to_string).collect();
        v.sort();
        v
    };
    let mut out = Vec::new();

    let signups: Vec<(String, String)> = sqlx::query_as(
        "SELECT signup_ip_hash, group_concat(username) FROM accounts
         WHERE signup_ip_hash IS NOT NULL AND created_at >= ?1 AND deleted_at IS NULL
         GROUP BY signup_ip_hash HAVING COUNT(*) >= ?2",
    )
    .bind(&month_ago)
    .bind(min)
    .fetch_all(db)
    .await?;
    out.extend(signups.into_iter().map(|(hash, names)| Cluster {
        why: format!("signed up from one address ({hash})"),
        usernames: split(names),
        serious: false,
    }));

    let guesses: Vec<(String, String)> = sqlx::query_as(
        "SELECT g.ip_hash, group_concat(DISTINCT a.username) FROM global_guesses g
         JOIN accounts a ON a.id = g.user_id
         WHERE g.ip_hash IS NOT NULL AND g.round_date >= ?1
         GROUP BY g.ip_hash HAVING COUNT(DISTINCT g.user_id) >= ?2",
    )
    .bind(&week_ago)
    .bind(min)
    .fetch_all(db)
    .await?;
    out.extend(guesses.into_iter().map(|(hash, names)| Cluster {
        why: format!("guessed from one address this week ({hash})"),
        usernames: split(names),
        serious: false,
    }));

    // Bursts, found in Rust: the timestamps are text, and a sliding window is
    // easier to say here than in SQL.
    let recent: Vec<(String, String)> = sqlx::query_as(
        "SELECT username, created_at FROM accounts
         WHERE intra_id IS NULL AND created_at >= ?1 AND deleted_at IS NULL
         ORDER BY created_at",
    )
    .bind(&month_ago)
    .fetch_all(db)
    .await?;
    let times: Vec<(String, DateTime<Utc>)> = recent
        .into_iter()
        .filter_map(|(name, at)| Some((name, DateTime::parse_from_rfc3339(&at).ok()?.with_timezone(&Utc))))
        .collect();
    let mut i = 0;
    while i < times.len() {
        let mut j = i;
        while j + 1 < times.len() && times[j + 1].1 - times[i].1 <= chrono::Duration::minutes(10) {
            j += 1;
        }
        if (j - i + 1) as i64 >= min {
            out.push(Cluster {
                why: format!("signed up within ten minutes ({})", times[i].1.format("%Y-%m-%d %H:%M UTC")),
                usernames: split(times[i..=j].iter().map(|t| t.0.clone()).collect::<Vec<_>>().join(",")),
                serious: false,
            });
            i = j + 1;
        } else {
            i += 1;
        }
    }

    // A winner helped by their own accounts: in a closed round, players on
    // the winner's network took numbers below the winning one, so the
    // winner's number was the lowest left unique. The trick a crowd of fake
    // accounts exists for. Closed rounds only: today's guesses stay secret,
    // from moderators too.
    let blocked: Vec<(String, String, i64, String)> = sqlx::query_as(
        "SELECT p.round_date, wa.username, w.value,
                group_concat(ba.username || ' (' || b.value || ')', ', ')
         FROM pots p
         JOIN global_guesses w ON w.round_date = p.round_date AND w.user_id = p.winner_id
         JOIN accounts wa ON wa.id = w.user_id
         JOIN global_guesses b ON b.round_date = p.round_date AND b.user_id != w.user_id
                              AND b.ip_hash = w.ip_hash AND b.value < w.value AND b.participates = 1
         JOIN accounts ba ON ba.id = b.user_id
         WHERE p.round_date >= ?1 AND p.voided = 0 AND w.ip_hash IS NOT NULL
         GROUP BY p.round_date, wa.username, w.value
         ORDER BY p.round_date DESC",
    )
    .bind(&month_ago[..10])
    .fetch_all(db)
    .await?;
    for (round, winner, value, blockers) in blocked {
        let mut names = vec![winner.clone()];
        names.extend(blockers.split(", ").filter_map(|b| b.split(' ').next()).map(str::to_string));
        out.push(Cluster {
            why: format!("round {round}: {winner} won with {value}; on the same network, {blockers} took lower numbers"),
            usernames: names,
            serious: true,
        });
    }

    // Accounts in lockstep: guessing within ten seconds of each other, round
    // after round. People on one network play at their own times; a script
    // driving several accounts does not.
    let pairs: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT a1.username, a2.username, COUNT(*) AS rounds
         FROM global_guesses g1
         JOIN global_guesses g2 ON g2.round_date = g1.round_date AND g2.user_id > g1.user_id
         JOIN accounts a1 ON a1.id = g1.user_id
         JOIN accounts a2 ON a2.id = g2.user_id
         WHERE g1.round_date >= ?1 AND g1.participates = 1 AND g2.participates = 1
           AND abs(julianday(g1.submitted_at) - julianday(g2.submitted_at)) * 86400 <= 10
         GROUP BY g1.user_id, g2.user_id
         HAVING rounds >= ?2",
    )
    .bind(&month_ago[..10])
    .bind(min)
    .fetch_all(db)
    .await?;
    for (names, rounds) in join_pairs(pairs) {
        out.push(Cluster {
            why: format!("guess within ten seconds of each other, in {rounds} rounds or more this month"),
            usernames: names,
            serious: true,
        });
    }

    // Number killers: the same two accounts the only two on a number below
    // the winner's, round after round. Two picks are all it takes to kill a
    // number, so that is what a blocking crowd spends; people who like 1
    // collide in crowds, not as the same pair alone. Network-blind, so it
    // catches accounts behind different connections too.
    let killers: Vec<(String, String, i64, String)> = sqlx::query_as(
        "WITH picks AS (
             SELECT round_date, value, COUNT(*) AS n FROM global_guesses
             WHERE participates = 1 AND round_date >= ?1 GROUP BY round_date, value
         )
         SELECT a1.username, a2.username, COUNT(*) AS rounds, group_concat(DISTINCT wa.username)
         FROM global_guesses g1
         JOIN global_guesses g2 ON g2.round_date = g1.round_date AND g2.value = g1.value
                               AND g2.user_id > g1.user_id AND g2.participates = 1
         JOIN picks c ON c.round_date = g1.round_date AND c.value = g1.value AND c.n = 2
         JOIN pots p ON p.round_date = g1.round_date AND p.voided = 0
         JOIN global_guesses w ON w.round_date = p.round_date AND w.user_id = p.winner_id AND w.value > g1.value
         JOIN accounts wa ON wa.id = w.user_id
         JOIN accounts a1 ON a1.id = g1.user_id
         JOIN accounts a2 ON a2.id = g2.user_id
         WHERE g1.round_date >= ?1 AND g1.participates = 1
         GROUP BY g1.user_id, g2.user_id
         HAVING rounds >= ?2",
    )
    .bind(&month_ago[..10])
    .bind(min)
    .fetch_all(db)
    .await?;
    let helped: std::collections::HashMap<(String, String), String> =
        killers.iter().map(|(a, b, _, w)| ((a.clone(), b.clone()), w.clone())).collect();
    for (names, rounds) in join_pairs(killers.iter().map(|(a, b, r, _)| (a.clone(), b.clone(), *r)).collect()) {
        let mut winners: Vec<&str> = helped
            .iter()
            .filter(|((a, _), _)| names.contains(a))
            .flat_map(|(_, w)| w.split(','))
            .collect();
        winners.sort_unstable();
        winners.dedup();
        out.push(Cluster {
            why: format!(
                "kill numbers in pairs: alone together on a number below the winner, {rounds} rounds or more this month (winners: {})",
                winners.join(", ")
            ),
            usernames: names,
            serious: true,
        });
    }

    // Signing in together: different accounts starting sessions within ten
    // seconds of each other, on several days. One person switching between
    // tabs does not; one script working through its accounts does.
    let logins: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT a1.username, a2.username, COUNT(DISTINCT substr(s1.created_at, 1, 10)) AS days
         FROM account_sessions s1
         JOIN account_sessions s2 ON s2.account_id > s1.account_id
              AND s2.created_at >= ?1
              AND abs(julianday(s1.created_at) - julianday(s2.created_at)) * 86400 <= 10
         JOIN accounts a1 ON a1.id = s1.account_id
         JOIN accounts a2 ON a2.id = s2.account_id
         WHERE s1.created_at >= ?1
         GROUP BY s1.account_id, s2.account_id
         HAVING days >= ?2",
    )
    .bind(&month_ago)
    .bind(min)
    .fetch_all(db)
    .await?;
    for (names, days) in join_pairs(logins) {
        out.push(Cluster {
            why: format!("sign in within ten seconds of each other, on {days} days or more this month"),
            usernames: names,
            serious: true,
        });
    }

    // Clockwork: guessing at the same time of day, day after day, as a
    // scheduled script does. Alone it may be a habit; several accounts on
    // the same clock are one script.
    let times: Vec<(String, String)> = sqlx::query_as(
        "SELECT a.username, g.submitted_at FROM global_guesses g
         JOIN accounts a ON a.id = g.user_id
         JOIN pots p ON p.round_date = g.round_date
         WHERE g.round_date >= ?1 AND g.participates = 1",
    )
    .bind(&month_ago[..10])
    .fetch_all(db)
    .await?;
    let mut by_account: std::collections::BTreeMap<String, Vec<i64>> = std::collections::BTreeMap::new();
    for (name, at) in times {
        if let Ok(t) = DateTime::parse_from_rfc3339(&at) {
            by_account.entry(name).or_default().push(i64::from(chrono::Timelike::num_seconds_from_midnight(&t.with_timezone(&Utc))));
        }
    }
    let mut clocks: Vec<(String, i64)> = by_account
        .into_iter()
        .filter_map(|(name, secs)| clockwork(&secs, CLOCK_WINDOW, CLOCK_DAYS).map(|at| (name, at)))
        .collect();
    clocks.sort_by_key(|c| c.1);
    let mut i = 0;
    while i < clocks.len() {
        let mut j = i;
        while j + 1 < clocks.len() && clocks[j + 1].1 - clocks[i].1 <= 60 {
            j += 1;
        }
        let at = clocks[i].1;
        let names: Vec<String> = clocks[i..=j].iter().map(|c| c.0.clone()).collect();
        out.push(Cluster {
            why: format!(
                "guess{} at about {:02}:{:02} UTC every day, {CLOCK_DAYS} days or more this month",
                if names.len() > 1 { "" } else { "es" },
                at / 3600,
                at % 3600 / 60
            ),
            serious: names.len() > 1,
            usernames: names,
        });
        i = j + 1;
    }

    out.sort_by_key(|c| !c.serious);
    Ok(out)
}

/// Guesses within this many seconds of the same time of day...
const CLOCK_WINDOW: i64 = 120;
/// ...on at least this many days make an account's guessing clockwork.
const CLOCK_DAYS: usize = 5;

/// If at least `days` of these times of day (seconds since midnight) fall
/// within `window` seconds of each other, the start of that window.
/// Midnight wraps: 23:59 and 00:01 are two minutes apart.
fn clockwork(secs: &[i64], window: i64, days: usize) -> Option<i64> {
    const DAY: i64 = 24 * 60 * 60;
    let mut s = secs.to_vec();
    s.sort_unstable();
    let n = s.len();
    let doubled: Vec<i64> = s.iter().copied().chain(s.iter().map(|t| t + DAY)).collect();
    (0..n)
        .find(|&i| doubled[i..i + n].iter().take_while(|t| **t - doubled[i] <= window).count() >= days)
        .map(|i| s[i])
}

/// Pairs joined up into groups: a, b and b, c are one group of three, with
/// the largest count among its pairs.
fn join_pairs(pairs: Vec<(String, String, i64)>) -> Vec<(Vec<String>, i64)> {
    let mut groups: Vec<(Vec<String>, i64)> = Vec::new();
    for (a, b, count) in pairs {
        let hit: Vec<usize> = (0..groups.len()).filter(|&i| groups[i].0.contains(&a) || groups[i].0.contains(&b)).collect();
        let mut merged = (vec![a, b], count);
        for &i in hit.iter().rev() {
            let (names, c) = groups.remove(i);
            merged.0.extend(names);
            merged.1 = merged.1.max(c);
        }
        merged.0.sort();
        merged.0.dedup();
        groups.push(merged);
    }
    groups
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
    verified: bool,
    points: i64,
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
                a.verified_at IS NOT NULL AS verified,
                (SELECT COALESCE(SUM(amount), 0) FROM ledger WHERE account_id = a.id) AS points,
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
                verified: r.verified,
            },
            intra,
            points: r.points,
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

/// How long a guess keeps the hash of the network it came from: long
/// enough for moderators to see a pattern, not for good.
pub const GUESS_IP_HASH_DAYS: i64 = 90;

/// Drop sessions, OAuth states and emailed links that have aged out, the
/// mail log past the days it is counted for, and guesses' network hashes
/// past `GUESS_IP_HASH_DAYS`.
pub async fn purge_expired(db: &Db) -> Result<()> {
    let now = ts(Utc::now());
    let hashes_from = (Utc::now() - chrono::Duration::days(GUESS_IP_HASH_DAYS)).format("%Y-%m-%d").to_string();
    sqlx::query("UPDATE global_guesses SET ip_hash = NULL WHERE ip_hash IS NOT NULL AND round_date < ?1")
        .bind(&hashes_from)
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM account_sessions WHERE expires_at <= ?1")
        .bind(&now)
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM oauth_states WHERE expires_at <= ?1")
        .bind(&now)
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM email_tokens WHERE expires_at <= ?1")
        .bind(&now)
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM mail_log WHERE sent_at <= ?1")
        .bind(ts(Utc::now() - chrono::Duration::days(2)))
        .execute(db)
        .await?;
    Ok(())
}

// --------------------------------------------------------- oauth states

/// A sign-in that has left for a provider and not come back yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthState {
    /// "42", "github" or "google": a state can only finish the flow it began.
    pub provider: String,
    /// Set when a signed-in account is adding this login, rather than
    /// someone signing in with it.
    pub link_account_id: Option<i64>,
    /// The PKCE verifier, for providers that take one.
    pub pkce_verifier: Option<String>,
}

pub async fn store_oauth_state(db: &Db, state: &str, flow: &OAuthState) -> Result<()> {
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO oauth_states (state, created_at, expires_at, provider, link_account_id, pkce_verifier)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(state)
    .bind(ts(now))
    .bind(ts(now + chrono::Duration::minutes(15)))
    .bind(&flow.provider)
    .bind(flow.link_account_id)
    .bind(&flow.pkce_verifier)
    .execute(db)
    .await?;
    Ok(())
}

/// The flow a state belongs to, if it was present and unexpired. Single-use:
/// the row is deleted in the same statement, whether or not it had expired.
pub async fn consume_oauth_state(db: &Db, state: &str) -> Result<Option<OAuthState>> {
    let now = ts(Utc::now());
    let row: Option<(String, Option<i64>, Option<String>, String)> = sqlx::query_as(
        "DELETE FROM oauth_states WHERE state = ?1
         RETURNING provider, link_account_id, pkce_verifier, expires_at",
    )
    .bind(state)
    .fetch_optional(db)
    .await?;
    Ok(row
        .filter(|r| r.3 > now)
        .map(|(provider, link_account_id, pkce_verifier, _)| OAuthState {
            provider,
            link_account_id,
            pkce_verifier,
        }))
}

// ----------------------------------------------------------- identities

/// The account a login belongs to, and whether it may sign in (not banned,
/// not deleted).
pub async fn identity_account(db: &Db, provider: &str, subject: &str) -> Result<Option<(i64, bool)>> {
    let row = sqlx::query_as(
        "SELECT a.id, a.banned_at IS NULL AND a.deleted_at IS NULL
         FROM identities i JOIN accounts a ON a.id = i.account_id
         WHERE i.provider = ?1 AND i.subject = ?2",
    )
    .bind(provider)
    .bind(subject)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

/// The account an email address belongs to, and whether that address was
/// ever proven: a confirmed password, or a provider that vouched for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailOwner {
    pub account_id: i64,
    pub proven: bool,
    /// Not banned or deleted.
    pub active: bool,
}

/// Who signs in with `email`, by password or through a provider. A proven
/// owner first, should two accounts share an address from before accounts
/// were joined up by email.
pub async fn email_owner(db: &Db, email: &str) -> Result<Option<EmailOwner>> {
    let row: Option<(i64, bool, bool)> = sqlx::query_as(
        "SELECT i.account_id,
                i.provider != 'email' OR p.verified_at IS NOT NULL AS proven,
                a.banned_at IS NULL AND a.deleted_at IS NULL AS active
         FROM identities i
         JOIN accounts a ON a.id = i.account_id
         LEFT JOIN passwords p ON p.account_id = i.account_id
         WHERE i.email = ?1 COLLATE NOCASE
         ORDER BY proven DESC, i.created_at
         LIMIT 1",
    )
    .bind(email)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(account_id, proven, active)| EmailOwner { account_id, proven, active }))
}

/// Takes away a password login whose address was never confirmed, because
/// someone who does own the address just proved it through a provider: a
/// stranger may have signed up with it to sit in wait (pre-hijacking). The
/// account goes with it if that was all it had, and it never played - an
/// unconfirmed account cannot. Otherwise only the login goes.
pub async fn drop_unproven_login(db: &Db, account_id: i64) -> Result<()> {
    let mut tx = write_tx(db).await?;
    let (others, played): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM identities WHERE account_id = ?1 AND provider != 'email'),
                (SELECT COUNT(*) FROM ledger WHERE account_id = ?1)
              + (SELECT COUNT(*) FROM global_guesses WHERE user_id = ?1)
              + (SELECT COUNT(*) FROM bets WHERE account_id = ?1)",
    )
    .bind(account_id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM passwords WHERE account_id = ?1 AND verified_at IS NULL")
        .bind(account_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM identities WHERE account_id = ?1 AND provider = 'email'")
        .bind(account_id)
        .execute(&mut *tx)
        .await?;
    if others == 0 && played == 0 {
        for sql in [
            "DELETE FROM account_sessions WHERE account_id = ?1",
            "DELETE FROM email_tokens WHERE account_id = ?1",
            "DELETE FROM accounts WHERE id = ?1 AND verified_at IS NULL",
        ] {
            sqlx::query(sql).bind(account_id).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// What came of adding a login to an account.
#[derive(Debug, PartialEq, Eq)]
pub enum Linked {
    Added,
    /// It was already this account's.
    Already,
    /// It belongs to someone else's account.
    Elsewhere,
}

/// Adds a login to `account_id`.
pub async fn link_identity(
    db: &Db,
    provider: &str,
    subject: &str,
    account_id: i64,
    email: Option<&str>,
) -> Result<Linked> {
    let inserted = sqlx::query(
        "INSERT INTO identities (provider, subject, account_id, email, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(provider, subject) DO NOTHING",
    )
    .bind(provider)
    .bind(subject)
    .bind(account_id)
    .bind(email)
    .bind(ts(Utc::now()))
    .execute(db)
    .await?
    .rows_affected();
    if inserted > 0 {
        return Ok(Linked::Added);
    }
    Ok(match identity_account(db, provider, subject).await? {
        Some((owner, _)) if owner == account_id => Linked::Already,
        _ => Linked::Elsewhere,
    })
}

/// A new account for someone signing in with a provider for the first time.
/// Verified at once: the provider vouches for them. The username is built on
/// `wanted` (already shaped by `credentials::username_from`), numbered until
/// it is free.
pub async fn create_provider_account(
    db: &Db,
    provider: &str,
    subject: &str,
    email: Option<&str>,
    wanted: &str,
    ip_hash: Option<&str>,
) -> Result<Account> {
    let mut tx = write_tx(db).await?;
    let now = ts(Utc::now());
    let mut username = wanted.to_string();
    for n in 2.. {
        let (taken,): (i64,) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM accounts WHERE username = ?1)
                  + (SELECT COUNT(*) FROM users WHERE login = ?1 COLLATE NOCASE)",
        )
        .bind(&username)
        .fetch_one(&mut *tx)
        .await?;
        if taken == 0 {
            break;
        }
        username = format!("{wanted}-{n}");
    }
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO accounts (username, created_at, last_seen_at, verified_at, signup_ip_hash)
         VALUES (?1, ?2, ?2, ?2, ?3) RETURNING id",
    )
    .bind(&username)
    .bind(&now)
    .bind(ip_hash)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO identities (provider, subject, account_id, email, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(provider)
    .bind(subject)
    .bind(id)
    .bind(email)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Account {
        id,
        username,
        intra_id: None,
        verified: true,
    })
}

/// Makes `account_id` play the 42 pool as `intra_id`, when a 42 login is
/// linked to an account that started elsewhere. Refused (false) if another
/// account already plays as that 42 user.
pub async fn attach_intra(db: &Db, account_id: i64, intra_id: i64) -> Result<bool> {
    let done = sqlx::query(
        "UPDATE accounts SET intra_id = ?1, verified_at = COALESCE(verified_at, ?3)
         WHERE id = ?2 AND (intra_id IS NULL OR intra_id = ?1)
           AND NOT EXISTS (SELECT 1 FROM accounts WHERE intra_id = ?1 AND id != ?2)",
    )
    .bind(intra_id)
    .bind(account_id)
    .bind(ts(Utc::now()))
    .execute(db)
    .await?
    .rows_affected();
    Ok(done > 0)
}

// ---------------------------------------------------------------- game

/// Records a guess. Returns `false` if this player already had one for the
/// round, leaving the original untouched. `player` is an intra id in the 42
/// pool and an account id in the global one.
///
/// Guesses are final, so this must never overwrite. `DO NOTHING` makes the
/// check and the insert one atomic statement: two requests racing each other
/// cannot both succeed, which a read-then-write pair could not guarantee.
pub async fn insert_guess(
    db: &Db,
    pool: Pool,
    round_date: &str,
    player: i64,
    value: i64,
    participates: bool,
) -> Result<bool> {
    let sql = format!(
        "INSERT INTO {} (round_date, user_id, value, submitted_at, participates)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(round_date, user_id) DO NOTHING",
        pool.guesses()
    );
    let inserted = sqlx::query(&sql)
        .bind(round_date)
        .bind(player)
        .bind(value)
        .bind(ts(Utc::now()))
        .bind(participates)
        .execute(db)
        .await?
        .rows_affected();
    Ok(inserted > 0)
}

/// Notes where a global guess came from (a keyed hash, see `signin::ip_hash`).
pub async fn set_guess_ip_hash(db: &Db, round_date: &str, account_id: i64, hash: &str) -> Result<()> {
    sqlx::query("UPDATE global_guesses SET ip_hash = ?1 WHERE round_date = ?2 AND user_id = ?3")
        .bind(hash)
        .bind(round_date)
        .bind(account_id)
        .execute(db)
        .await?;
    Ok(())
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

pub async fn round_guesses(db: &Db, pool: Pool, round_date: &str) -> Result<Vec<GuessRow>> {
    let sql = format!(
        "SELECT p.login, p.display_name, g.value, g.participates
         FROM {} g JOIN {} p ON p.id = g.user_id
         WHERE g.round_date = ?1
         ORDER BY g.participates DESC, g.value ASC",
        pool.guesses(),
        pool.players()
    );
    let rows = sqlx::query_as::<_, GuessRow>(&sql)
        .bind(round_date)
        .fetch_all(db)
        .await?;
    Ok(rows)
}

pub async fn clear_round(db: &Db, pool: Pool, round_date: &str) -> Result<u64> {
    let sql = format!("DELETE FROM {} WHERE round_date = ?1", pool.guesses());
    let n = sqlx::query(&sql)
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
///
/// The global pool is keyed by account, so there each bot also gets an
/// account linked to its `users` row; deleting the bots takes both away.
pub async fn insert_demo(db: &Db, pool: Pool, guesses: &[DemoGuess]) -> Result<u64> {
    let mut tx = write_tx(db).await?;
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
        let id = match pool {
            Pool::Vienna => id,
            Pool::Global => {
                sqlx::query(
                    "INSERT OR IGNORE INTO accounts
                         (username, intra_id, created_at, last_seen_at, verified_at)
                     VALUES (?1, ?2, ?3, ?3, ?3)",
                )
                .bind(&login)
                .bind(id)
                .bind(&now)
                .execute(&mut *tx)
                .await?;
                let account: Option<(i64,)> =
                    sqlx::query_as("SELECT id FROM accounts WHERE intra_id = ?1")
                        .bind(id)
                        .fetch_optional(&mut *tx)
                        .await?;
                match account {
                    Some((account,)) => account,
                    None => anyhow::bail!("the username {login} is taken by a real account"),
                }
            }
        };
        ids.push(id);
    }

    let insert = format!(
        "INSERT OR IGNORE INTO {} (round_date, user_id, value, submitted_at, participates)
         VALUES (?1, ?2, ?3, ?4, 1)",
        pool.guesses()
    );
    let mut added = 0;
    for g in guesses {
        added += sqlx::query(&insert)
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

/// Deletes every bot, and with them (ON DELETE CASCADE) every guess they
/// made. In the global round bots also hold points, bets and won pots,
/// which point at their accounts and do not cascade; and every round they
/// played was decided partly by them. So from the first round a bot played,
/// what was settled is undone (pots, winnings, bet results - rounds called
/// off stay as they are), the bots' own points and bets go, and the rounds
/// settle again with the real players alone.
pub async fn remove_demo(db: &Db) -> Result<u64> {
    const BOTS: &str = "SELECT a.id FROM accounts a JOIN users u ON u.id = a.intra_id
                        WHERE u.id < 0 AND u.login LIKE 'bot-%'";
    let mut tx = write_tx(db).await?;
    let first: Option<String> = sqlx::query_scalar(&format!(
        "SELECT MIN(round_date) FROM global_guesses WHERE user_id IN ({BOTS})"
    ))
    .fetch_one(&mut *tx)
    .await?;
    if let Some(from) = first {
        const CALLED_OFF: &str = "SELECT round_date FROM pots WHERE voided = 1";
        for sql in [
            format!(
                "DELETE FROM ledger WHERE round_date >= ?1 AND round_date NOT IN ({CALLED_OFF})
                   AND (kind IN ('pot', 'bet_win') OR (kind = 'refund' AND ref LIKE 'bet:%'))"
            ),
            format!(
                "UPDATE bets SET result = NULL, payout = NULL, settled_at = NULL
                 WHERE round_date >= ?1 AND round_date NOT IN ({CALLED_OFF})"
            ),
            "DELETE FROM pots WHERE round_date >= ?1 AND voided = 0".to_string(),
        ] {
            sqlx::query(&sql).bind(&from).execute(&mut *tx).await?;
        }
    }
    for table in ["ledger", "bets"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE account_id IN ({BOTS})"))
            .execute(&mut *tx)
            .await?;
    }
    let n = sqlx::query("DELETE FROM users WHERE id < 0 AND login LIKE 'bot-%'")
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(n)
}

/// How many bots exist and how many guesses they hold, in both pools.
pub async fn demo_counts(db: &Db) -> Result<(i64, i64)> {
    let counts: (i64, i64) = sqlx::query_as(
        "SELECT
             (SELECT COUNT(*) FROM users WHERE id < 0 AND login LIKE 'bot-%'),
             (SELECT COUNT(*) FROM guesses g JOIN users u ON u.id = g.user_id
              WHERE u.id < 0 AND u.login LIKE 'bot-%')
           + (SELECT COUNT(*) FROM global_guesses g
              JOIN accounts a ON a.id = g.user_id
              JOIN users u ON u.id = a.intra_id
              WHERE u.id < 0 AND u.login LIKE 'bot-%')",
    )
    .fetch_one(db)
    .await?;
    Ok(counts)
}

pub async fn my_guess(db: &Db, pool: Pool, round_date: &str, player: i64) -> Result<Option<i64>> {
    let sql = format!(
        "SELECT value FROM {} WHERE round_date = ?1 AND user_id = ?2",
        pool.guesses()
    );
    let value: Option<(i64,)> = sqlx::query_as(&sql)
        .bind(round_date)
        .bind(player)
        .fetch_optional(db)
        .await?;
    Ok(value.map(|v| v.0))
}

pub async fn guess_count(db: &Db, pool: Pool, round_date: &str) -> Result<i64> {
    let sql = format!(
        "SELECT COUNT(*) FROM {} WHERE round_date = ?1 AND participates = 1",
        pool.guesses()
    );
    let (n,): (i64,) = sqlx::query_as(&sql)
        .bind(round_date)
        .fetch_one(db)
        .await?;
    Ok(n)
}

/// The winner of a round is the *lowest value that exactly one player picked*.
/// A round with no such value has no winner.
fn winner_cte(pool: Pool) -> String {
    let g = pool.guesses();
    format!(
        "
    WITH rounds AS (
        SELECT round_date, COUNT(*) AS total
        FROM {g} WHERE round_date < ?1 AND participates = 1
        GROUP BY round_date
    ),
    uniq AS (
        SELECT round_date, value
        FROM {g} WHERE round_date < ?1 AND participates = 1
        GROUP BY round_date, value
        HAVING COUNT(*) = 1
    ),
    winners AS (
        SELECT round_date, MIN(value) AS value FROM uniq GROUP BY round_date
    )
"
    )
}

/// The winner CTE, then every closed round with its winner's name; callers
/// add their own WHERE / ORDER / LIMIT.
fn summaries(pool: Pool) -> String {
    format!(
        "{}
         SELECT r.round_date, r.total,
                w.value    AS winning_value,
                p.id       AS winner_id,
                p.login    AS winner_login,
                p.display_name AS winner_name
         FROM rounds r
         LEFT JOIN winners w ON w.round_date = r.round_date
         LEFT JOIN {} g ON g.round_date = w.round_date AND g.value = w.value
                       AND g.participates = 1
         LEFT JOIN {} p ON p.id = g.user_id",
        winner_cte(pool),
        pool.guesses(),
        pool.players()
    )
}

/// Closed rounds, newest first. `open_round` is the round still taking
/// guesses; everything strictly before it is closed.
pub async fn closed_rounds(
    db: &Db,
    pool: Pool,
    open_round: &str,
    limit: i64,
) -> Result<Vec<RoundSummary>> {
    let sql = format!("{} ORDER BY r.round_date DESC LIMIT ?2", summaries(pool));
    let rows = sqlx::query_as::<_, RoundSummary>(&sql)
        .bind(open_round)
        .bind(limit)
        .fetch_all(db)
        .await?;
    Ok(rows)
}

/// Summary for one specific closed round.
pub async fn round_summary(
    db: &Db,
    pool: Pool,
    open_round: &str,
    date: &str,
) -> Result<Option<RoundSummary>> {
    let sql = format!("{} WHERE r.round_date = ?2", summaries(pool));
    let row = sqlx::query_as::<_, RoundSummary>(&sql)
        .bind(open_round)
        .bind(date)
        .fetch_optional(db)
        .await?;
    Ok(row)
}

/// How many rounds have closed. A count rather than a list: the day page only
/// wants to say what its own short list leaves out.
pub async fn closed_round_count(db: &Db, pool: Pool, open_round: &str) -> Result<i64> {
    let sql = format!(
        "SELECT COUNT(DISTINCT round_date) FROM {}
         WHERE round_date < ?1 AND participates = 1",
        pool.guesses()
    );
    let n: i64 = sqlx::query_scalar(&sql)
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
    pool: Pool,
    open_round: &str,
    date: &str,
    limit: i64,
) -> Result<Vec<RoundSummary>> {
    let sql = format!(
        "{} WHERE r.round_date < ?2 ORDER BY r.round_date DESC LIMIT ?3",
        summaries(pool)
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
pub async fn round_after(
    db: &Db,
    pool: Pool,
    open_round: &str,
    date: &str,
) -> Result<Option<String>> {
    let sql = format!(
        "SELECT MIN(round_date) FROM {}
         WHERE round_date > ?1 AND round_date < ?2 AND participates = 1",
        pool.guesses()
    );
    let row: Option<Option<String>> = sqlx::query_scalar(&sql)
        .bind(date)
        .bind(open_round)
        .fetch_optional(db)
        .await?;
    Ok(row.flatten())
}

/// How many players picked each value in a round, lowest value first. Ghosts
/// are left out, as everywhere else in the game. Callers must only ask about
/// closed rounds - this does not check, and an open round's numbers are secret.
pub async fn round_tallies(db: &Db, pool: Pool, round_date: &str) -> Result<Vec<Tally>> {
    let sql = format!(
        "SELECT value, COUNT(*) AS count
         FROM {} WHERE round_date = ?1 AND participates = 1
         GROUP BY value ORDER BY value",
        pool.guesses()
    );
    let rows = sqlx::query_as::<_, Tally>(&sql)
        .bind(round_date)
        .fetch_all(db)
        .await?;
    Ok(rows)
}

/// Every closed pick in a pool, all time, as (number, times picked): the
/// typical crowd the odds lean on. Stops before `open_round`, which is
/// secret.
pub async fn all_time_tallies(db: &Db, pool: Pool, open_round: &str) -> Result<Vec<(i64, i64)>> {
    let sql = format!(
        "SELECT value, COUNT(*) FROM {} WHERE round_date < ?1 AND participates = 1 GROUP BY value",
        pool.guesses()
    );
    Ok(sqlx::query_as(&sql).bind(open_round).fetch_all(db).await?)
}

/// Per-day tallies for every closed round from `from` (inclusive) up to the
/// open round (exclusive), oldest day first. Ghosts are left out. Stopping
/// before `open_round` is what keeps the open round's numbers secret.
pub async fn range_tallies(
    db: &Db,
    pool: Pool,
    from: &str,
    open_round: &str,
) -> Result<Vec<DayTally>> {
    let sql = format!(
        "SELECT round_date, value, COUNT(*) AS count
         FROM {}
         WHERE round_date >= ?1 AND round_date < ?2 AND participates = 1
         GROUP BY round_date, value
         ORDER BY round_date, value",
        pool.guesses()
    );
    let rows = sqlx::query_as::<_, DayTally>(&sql)
        .bind(from)
        .bind(open_round)
        .fetch_all(db)
        .await?;
    Ok(rows)
}

pub async fn leaderboard(
    db: &Db,
    pool: Pool,
    open_round: &str,
    limit: i64,
) -> Result<Vec<LeaderboardRow>> {
    let sql = format!(
        "{}
         SELECT p.id, p.login, p.display_name, COUNT(*) AS wins
         FROM winners w
         JOIN {} g ON g.round_date = w.round_date AND g.value = w.value
                   AND g.participates = 1
         JOIN {} p ON p.id = g.user_id
         GROUP BY p.id
         ORDER BY wins DESC, p.login ASC
         LIMIT ?2",
        winner_cte(pool),
        pool.guesses(),
        pool.players()
    );
    let rows = sqlx::query_as::<_, LeaderboardRow>(&sql)
        .bind(open_round)
        .bind(limit)
        .fetch_all(db)
        .await?;
    Ok(rows)
}

/// Every day each player of `pool` played, the open round included, as
/// (player id, login, display name, round date), by player and then date:
/// what the streak leaderboard is worked out from. Guesses that do not
/// count (ghosts, test accounts, banned) are not days played.
pub async fn play_days(db: &Db, pool: Pool) -> Result<Vec<(i64, String, String, String)>> {
    let sql = format!(
        "SELECT p.id, p.login, p.display_name, g.round_date
         FROM {} g JOIN {} p ON p.id = g.user_id
         WHERE g.participates = 1
         ORDER BY p.id, g.round_date",
        pool.guesses(),
        pool.players()
    );
    Ok(sqlx::query_as(&sql).fetch_all(db).await?)
}

/// One closed round a player took part in: what they picked, and what won.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct PlayedRound {
    pub round_date: String,
    pub value: i64,
    pub winning_value: Option<i64>,
}

impl PlayedRound {
    /// The winning number is picked by exactly one player, so whoever picked
    /// it is the winner.
    pub fn won(&self) -> bool {
        self.winning_value == Some(self.value)
    }
}

/// Every closed round `player` guessed in, newest first. Ghost guesses are
/// left out, as everywhere. Read from guesses as they are: no new table.
pub async fn played_rounds(db: &Db, pool: Pool, open_round: &str, player: i64) -> Result<Vec<PlayedRound>> {
    let sql = format!(
        "{}
         SELECT g.round_date, g.value, w.value AS winning_value
         FROM {} g LEFT JOIN winners w ON w.round_date = g.round_date
         WHERE g.user_id = ?2 AND g.round_date < ?1 AND g.participates = 1
         ORDER BY g.round_date DESC",
        winner_cte(pool),
        pool.guesses()
    );
    let rows = sqlx::query_as::<_, PlayedRound>(&sql)
        .bind(open_round)
        .bind(player)
        .fetch_all(db)
        .await?;
    Ok(rows)
}

/// Whether any generated demo bot played in `pool`: their rounds are seeded
/// data, not people.
pub async fn has_demo_data(db: &Db, pool: Pool) -> Result<bool> {
    let sql = match pool {
        Pool::Vienna => "SELECT EXISTS(SELECT 1 FROM guesses g JOIN users u ON u.id = g.user_id
                          WHERE u.id < 0 AND u.login LIKE 'bot-%')",
        Pool::Global => "SELECT EXISTS(SELECT 1 FROM global_guesses g JOIN accounts a ON a.id = g.user_id
                          JOIN users u ON u.id = a.intra_id WHERE u.id < 0 AND u.login LIKE 'bot-%')",
    };
    let (yes,): (bool,) = sqlx::query_as(sql).fetch_one(db).await?;
    Ok(yes)
}

// --------------------------------------------------------------- points

/// An account's points: the sum of its ledger.
pub async fn balance(db: &Db, account_id: i64) -> Result<i64> {
    let (n,): (i64,) = sqlx::query_as("SELECT COALESCE(SUM(amount), 0) FROM ledger WHERE account_id = ?1")
        .bind(account_id)
        .fetch_one(db)
        .await?;
    Ok(n)
}

/// Writes a ledger row unless one with the same (account, kind, ref) is
/// there already. True if it was written: the way every credit and charge
/// happens at most once.
pub async fn credit_once(
    db: &Db,
    account_id: i64,
    round_date: &str,
    kind: &str,
    amount: i64,
    reference: &str,
    note: Option<&str>,
) -> Result<bool> {
    let n = sqlx::query(
        "INSERT INTO ledger (account_id, round_date, kind, amount, ref, note, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(account_id, kind, ref) DO NOTHING",
    )
    .bind(account_id)
    .bind(round_date)
    .bind(kind)
    .bind(amount)
    .bind(reference)
    .bind(note)
    .bind(ts(Utc::now()))
    .execute(db)
    .await?
    .rows_affected();
    Ok(n > 0)
}

/// The round an account got its welcome points in: when it joined the
/// global round. None before it has.
pub async fn joined_global(db: &Db, account_id: i64) -> Result<Option<String>> {
    Ok(sqlx::query_scalar("SELECT round_date FROM ledger WHERE account_id = ?1 AND kind = 'signup'")
        .bind(account_id)
        .fetch_optional(db)
        .await?)
}

/// How many global rounds were played before `round_date`: days with at
/// least one real guess.
pub async fn global_rounds_before(db: &Db, round_date: &str) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(DISTINCT round_date) FROM global_guesses WHERE round_date < ?1 AND participates = 1")
        .bind(round_date)
        .fetch_one(db)
        .await?)
}

/// What an account has had of one kind of ledger row, all told.
pub async fn ledger_total(db: &Db, account_id: i64, kind: &str) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT COALESCE(SUM(amount), 0) FROM ledger WHERE account_id = ?1 AND kind = ?2")
        .bind(account_id)
        .bind(kind)
        .fetch_one(db)
        .await?)
}

/// Every day an account has a guess in the global round, the open one
/// included, oldest first. Ghost guesses do not count.
pub async fn global_play_days(db: &Db, account_id: i64) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT round_date FROM global_guesses WHERE user_id = ?1 AND participates = 1 ORDER BY round_date",
    )
    .bind(account_id)
    .fetch_all(db)
    .await?)
}

/// The refs of every ledger row of `kind` an account has, e.g. the streak
/// bonuses it claimed.
pub async fn ledger_refs(db: &Db, account_id: i64, kind: &str) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar("SELECT ref FROM ledger WHERE account_id = ?1 AND kind = ?2")
        .bind(account_id)
        .bind(kind)
        .fetch_all(db)
        .await?)
}

/// What came of locking a guess in the global round.
#[derive(Debug, PartialEq, Eq)]
pub enum Locked {
    /// In, and the stake taken.
    In,
    /// The same number was already in: nothing more to do.
    AlreadySame,
    /// A different number was already in; it stands.
    AlreadyOther,
    /// Not enough points for the stake. Nothing was written.
    Broke,
    /// A moderator called the round off; it takes no more guesses.
    Voided,
}

/// Accounts whose username starts with "test", in any case, are the
/// maintainers' own test accounts on the live site: their guesses are
/// ghosts (`participates = 0`, no stake), so they never win, fill the pot,
/// or show in results, trends or odds; they are left off the points
/// leaderboard, and their bets do not move the odds. A SQL condition on a
/// username column; LIKE is case-insensitive for ASCII.
pub const TEST_ACCOUNTS: &str = "LIKE 'test%'";

/// Locks a guess in the global round, takes its stake and credits the day's
/// `grant` (points for playing, not for visiting), together or not at all.
/// The stake goes into the round's pot (see `settle_round`).
pub async fn lock_global_guess(
    db: &Db,
    round_date: &str,
    account_id: i64,
    value: i64,
    stake: i64,
    grant: i64,
) -> Result<Locked> {
    let mut tx = write_tx(db).await?;
    let voided: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM pots WHERE round_date = ?1")
        .bind(round_date)
        .fetch_optional(&mut *tx)
        .await?;
    if voided.is_some() {
        return Ok(Locked::Voided);
    }
    let existing: Option<(i64,)> =
        sqlx::query_as("SELECT value FROM global_guesses WHERE round_date = ?1 AND user_id = ?2")
            .bind(round_date)
            .bind(account_id)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some((v,)) = existing {
        return Ok(if v == value { Locked::AlreadySame } else { Locked::AlreadyOther });
    }
    let (balance,): (i64,) = sqlx::query_as("SELECT COALESCE(SUM(amount), 0) FROM ledger WHERE account_id = ?1")
        .bind(account_id)
        .fetch_one(&mut *tx)
        .await?;
    // A test account plays for free and out of sight (`TEST_ACCOUNTS`).
    let (test,): (bool,) = sqlx::query_as(&format!("SELECT username {TEST_ACCOUNTS} FROM accounts WHERE id = ?1"))
        .bind(account_id)
        .fetch_one(&mut *tx)
        .await?;
    let stake = if test { 0 } else { stake };
    // The day's grant comes with the guess, so it counts towards the stake.
    if balance + grant < stake {
        return Ok(Locked::Broke);
    }
    let now = ts(Utc::now());
    let inserted = sqlx::query(
        "INSERT INTO global_guesses (round_date, user_id, value, submitted_at, participates)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(round_date, user_id) DO NOTHING",
    )
    .bind(round_date)
    .bind(account_id)
    .bind(value)
    .bind(&now)
    .bind(!test)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted == 0 {
        // Another request got in between; whatever it locked stands.
        return Ok(Locked::AlreadyOther);
    }
    if stake > 0 {
        sqlx::query(
            "INSERT INTO ledger (account_id, round_date, kind, amount, ref, created_at)
             VALUES (?1, ?2, 'stake', ?3, ?2, ?4)",
        )
        .bind(account_id)
        .bind(round_date)
        .bind(-stake)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }
    if grant > 0 {
        sqlx::query(
            "INSERT INTO ledger (account_id, round_date, kind, amount, ref, created_at)
             VALUES (?1, ?2, 'grant', ?3, ?2, ?4)
             ON CONFLICT(account_id, kind, ref) DO NOTHING",
        )
        .bind(account_id)
        .bind(round_date)
        .bind(grant)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Locked::In)
}

/// The stakes in a round so far, net of any refunded.
async fn round_stakes(conn: &mut sqlx::SqliteConnection, round_date: &str) -> Result<i64> {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(-SUM(amount), 0) FROM ledger
         WHERE round_date = ?1 AND (kind = 'stake' OR (kind = 'refund' AND ref LIKE 'stake:%'))",
    )
    .bind(round_date)
    .fetch_one(&mut *conn)
    .await?;
    Ok(n)
}

/// What rolls into `round_date` from the last settled round before it: all
/// of its pot if nobody won it (or it was voided), nothing if somebody did.
async fn carry_into(conn: &mut sqlx::SqliteConnection, round_date: &str) -> Result<i64> {
    let last: Option<(i64, i64, Option<i64>)> = sqlx::query_as(
        "SELECT stakes, carried_in, winner_id FROM pots
         WHERE round_date < ?1 ORDER BY round_date DESC LIMIT 1",
    )
    .bind(round_date)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(match last {
        Some((stakes, carried, None)) => stakes + carried,
        _ => 0,
    })
}

/// The open round's pot so far: its stakes and what rolled into it.
pub async fn pot_so_far(db: &Db, round_date: &str) -> Result<(i64, i64)> {
    let mut conn = db.acquire().await?;
    let stakes = round_stakes(&mut conn, round_date).await?;
    let carried = carry_into(&mut conn, round_date).await?;
    Ok((stakes, carried))
}

/// A settled round's pot, as the day page shows it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Pot {
    pub round_date: String,
    pub stakes: i64,
    pub carried_in: i64,
    pub winner_id: Option<i64>,
    pub paid: i64,
    pub players: i64,
    pub voided: bool,
}

pub async fn pot(db: &Db, round_date: &str) -> Result<Option<Pot>> {
    let row = sqlx::query_as::<_, Pot>(
        "SELECT round_date, stakes, carried_in, winner_id, paid, players, voided FROM pots WHERE round_date = ?1",
    )
    .bind(round_date)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

/// Global rounds before `open_round` with guesses or stakes and no pot row
/// yet, oldest first: settling goes in order, since each carries into the
/// next.
pub async fn unsettled_rounds(db: &Db, open_round: &str) -> Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT round_date FROM (
             SELECT round_date FROM global_guesses WHERE round_date < ?1
             UNION SELECT round_date FROM ledger WHERE kind = 'stake' AND round_date < ?1
             UNION SELECT round_date FROM bets WHERE round_date < ?1
         )
         WHERE round_date NOT IN (SELECT round_date FROM pots)
         ORDER BY round_date",
    )
    .bind(open_round)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

/// Settles one closed global round, all in one transaction: the winner (the
/// lowest number exactly one real player picked) takes the stakes plus
/// whatever rolled in; with no winner the lot rolls on. The pots row is the
/// "settled" mark, and the ledger's uniqueness means a retry after a crash
/// pays nothing twice. `None` if it was settled already.
pub async fn settle_round(db: &Db, round_date: &str) -> Result<Option<Pot>> {
    let mut tx = write_tx(db).await?;
    let done: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM pots WHERE round_date = ?1")
        .bind(round_date)
        .fetch_optional(&mut *tx)
        .await?;
    if done.is_some() {
        return Ok(None);
    }
    let winner: Option<(i64, i64)> = sqlx::query_as(
        "SELECT g.user_id, g.value FROM global_guesses g
         WHERE g.round_date = ?1 AND g.participates = 1 AND g.value = (
             SELECT value FROM global_guesses WHERE round_date = ?1 AND participates = 1
             GROUP BY value HAVING COUNT(*) = 1 ORDER BY value LIMIT 1
         )",
    )
    .bind(round_date)
    .fetch_optional(&mut *tx)
    .await?;
    let (players,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM global_guesses WHERE round_date = ?1 AND participates = 1")
            .bind(round_date)
            .fetch_one(&mut *tx)
            .await?;
    // The most picked number; on a tie, the lowest of them.
    let most: Option<(i64,)> = sqlx::query_as(
        "SELECT value FROM global_guesses WHERE round_date = ?1 AND participates = 1
         GROUP BY value ORDER BY COUNT(*) DESC, value LIMIT 1",
    )
    .bind(round_date)
    .fetch_optional(&mut *tx)
    .await?;
    let stakes = round_stakes(&mut tx, round_date).await?;
    let carried_in = carry_into(&mut tx, round_date).await?;
    let winner_id = winner.map(|w| w.0);
    let winning_value = winner.map(|w| w.1);
    let paid = if winner_id.is_some() { stakes + carried_in } else { 0 };
    let now = ts(Utc::now());
    // The round's bets, against its winning and most picked numbers. A round
    // nobody played has no outcome to bet on: its bets are called off.
    let outcome = crate::odds::Outcome { winner: winning_value, most: most.map(|m| m.0) };
    settle_bets(&mut tx, round_date, (players > 0).then_some(outcome), &now).await?;
    if let Some(w) = winner_id.filter(|_| paid > 0) {
        sqlx::query(
            "INSERT INTO ledger (account_id, round_date, kind, amount, ref, created_at)
             VALUES (?1, ?2, 'pot', ?3, ?2, ?4)
             ON CONFLICT(account_id, kind, ref) DO NOTHING",
        )
        .bind(w)
        .bind(round_date)
        .bind(paid)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "INSERT INTO pots (round_date, stakes, carried_in, winner_id, paid, players, settled_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )
    .bind(round_date)
    .bind(stakes)
    .bind(carried_in)
    .bind(winner_id)
    .bind(paid)
    .bind(players)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(Pot {
        round_date: round_date.to_string(),
        stakes,
        carried_in,
        winner_id,
        paid,
        players,
        voided: false,
    }))
}

/// Undoes the settlement of every round from `open_round` on: a test
/// instance's clock was moved back, so those rounds are open again and
/// should never have paid. Pots and bet winnings are taken back, bets
/// reopen (their void refunds taken back too), and daily points and streak
/// bonuses for days still to come are removed. Guesses and stakes stay: the
/// rounds settle again, properly, when they close. Returns the rounds undone.
pub async fn unsettle_from(db: &Db, open_round: &str) -> Result<u64> {
    // A round a moderator called off stays called off: its refunds stand.
    const CALLED_OFF: &str = "SELECT round_date FROM pots WHERE voided = 1";
    let mut tx = write_tx(db).await?;
    sqlx::query(&format!(
        "DELETE FROM ledger WHERE round_date >= ?1 AND (
             (kind IN ('pot', 'bet_win') AND round_date NOT IN ({CALLED_OFF}))
             OR (kind = 'refund' AND ref LIKE 'bet:%' AND round_date NOT IN ({CALLED_OFF}))
             OR (kind IN ('grant', 'streak', 'catchup') AND round_date > ?1))"
    ))
    .bind(open_round)
    .execute(&mut *tx)
    .await?;
    sqlx::query(&format!(
        "UPDATE bets SET result = NULL, payout = NULL, settled_at = NULL
         WHERE round_date >= ?1 AND round_date NOT IN ({CALLED_OFF})"
    ))
    .bind(open_round)
    .execute(&mut *tx)
    .await?;
    let undone = sqlx::query("DELETE FROM pots WHERE round_date >= ?1 AND voided = 0")
        .bind(open_round)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(undone)
}

/// Calls off a round that has not been settled: every stake refunded, no
/// guess counts, no pot paid, and what had rolled in rolls on. False if it
/// was settled already.
pub async fn void_round(db: &Db, round_date: &str) -> Result<bool> {
    let mut tx = write_tx(db).await?;
    let done: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM pots WHERE round_date = ?1")
        .bind(round_date)
        .fetch_optional(&mut *tx)
        .await?;
    if done.is_some() {
        return Ok(false);
    }
    let now = ts(Utc::now());
    sqlx::query(
        "INSERT INTO ledger (account_id, round_date, kind, amount, ref, note, created_at)
         SELECT account_id, round_date, 'refund', -amount, 'stake:' || round_date, 'round voided', ?2
         FROM ledger WHERE round_date = ?1 AND kind = 'stake'
         ON CONFLICT(account_id, kind, ref) DO NOTHING",
    )
    .bind(round_date)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE global_guesses SET participates = 0 WHERE round_date = ?1")
        .bind(round_date)
        .execute(&mut *tx)
        .await?;
    settle_bets(&mut tx, round_date, None, &now).await?;
    let carried_in = carry_into(&mut tx, round_date).await?;
    sqlx::query(
        "INSERT INTO pots (round_date, stakes, carried_in, winner_id, paid, players, voided, settled_at)
         VALUES (?1, 0, ?2, NULL, 0, 0, 1, ?3)",
    )
    .bind(round_date)
    .bind(carried_in)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

// ----------------------------------------------------------------- bets

/// A bet as pages show it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Bet {
    pub id: i64,
    pub round_date: String,
    pub market: String,
    pub stake: i64,
    pub odds_x100: i64,
    pub result: Option<String>,
    pub payout: Option<i64>,
    /// The most it can win over its stake; None for bets from before caps.
    pub max_profit: Option<i64>,
}

/// What came of placing a bet.
#[derive(Debug, PartialEq, Eq)]
pub enum Placed {
    /// In, and the stake taken.
    In,
    /// One bet per market per round; this one is in already.
    Already,
    /// Not a stake: under 1.
    BadStake,
    /// Not enough points for the stake.
    Broke,
    /// With the account's other bets on the round, it could win more than
    /// the cap allows for all of them together.
    OverRoundCap,
    /// Enough for the stake, but not for that and today's guess after it:
    /// the account has not guessed yet and must be able to.
    KeepForGuess,
    /// The round is settled or called off.
    Closed,
    /// The odds moved below what the visitor was shown, or the bet is no
    /// longer offered.
    Moved,
}

/// Places a bet and takes the stake, in one transaction. Any stake from 1
/// up to the account's points - less `keep` while the account has not
/// guessed in the round yet, so a bet never costs anyone their guess.
/// `price` is given the round's open bets
/// (market, stake, account) as they stand inside the transaction, and returns the
/// odds to place at, or None to refuse: so two bets at once each see the
/// other, and nobody gets odds the book has already moved past.
/// What an account's open bets on a round could win together, over their
/// stakes, each at most its own cap.
async fn open_profit(conn: &mut sqlx::SqliteConnection, account_id: i64, round_date: &str) -> Result<i64> {
    let bets: Vec<(i64, i64, Option<i64>)> = sqlx::query_as(
        "SELECT stake, odds_x100, max_profit FROM bets WHERE account_id = ?1 AND round_date = ?2 AND result IS NULL",
    )
    .bind(account_id)
    .bind(round_date)
    .fetch_all(&mut *conn)
    .await?;
    Ok(bets.into_iter().map(|(s, o, cap)| crate::odds::payout(s, o, cap) - s).sum())
}

/// What `account_id` could still win on `round_date` under a cap of
/// `max_profit` for all its bets together.
pub async fn profit_room(db: &Db, account_id: i64, round_date: &str, max_profit: i64) -> Result<i64> {
    let mut conn = db.acquire().await?;
    Ok((max_profit - open_profit(&mut conn, account_id, round_date).await?).max(0))
}

/// What a bet may not go past: what all the account's bets on the round may
/// win together over their stakes, and the points to keep for a guess not
/// made yet.
#[derive(Debug, Clone, Copy)]
pub struct BetLimits {
    pub max_profit: i64,
    pub keep: i64,
}

pub async fn place_bet(
    db: &Db,
    account_id: i64,
    round_date: &str,
    market: &str,
    stake: i64,
    limits: BetLimits,
    price: impl FnOnce(&[(String, i64, i64)]) -> Option<i64>,
) -> Result<Placed> {
    if stake < 1 {
        return Ok(Placed::BadStake);
    }
    let mut tx = write_tx(db).await?;
    let closed: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM pots WHERE round_date = ?1")
        .bind(round_date)
        .fetch_optional(&mut *tx)
        .await?;
    if closed.is_some() {
        return Ok(Placed::Closed);
    }
    let (already,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM bets WHERE account_id = ?1 AND round_date = ?2 AND market = ?3 AND result IS NULL",
    )
    .bind(account_id)
    .bind(round_date)
    .bind(market)
    .fetch_one(&mut *tx)
    .await?;
    if already > 0 {
        return Ok(Placed::Already);
    }
    let (balance,): (i64,) = sqlx::query_as("SELECT COALESCE(SUM(amount), 0) FROM ledger WHERE account_id = ?1")
        .bind(account_id)
        .fetch_one(&mut *tx)
        .await?;
    if balance < stake {
        return Ok(Placed::Broke);
    }
    let guessed: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM global_guesses WHERE round_date = ?1 AND user_id = ?2")
        .bind(round_date)
        .bind(account_id)
        .fetch_optional(&mut *tx)
        .await?;
    if guessed.is_none() && balance - stake < limits.keep {
        return Ok(Placed::KeepForGuess);
    }
    let book: Vec<(String, i64, i64)> = sqlx::query_as("SELECT market, stake, account_id FROM bets WHERE round_date = ?1 AND result IS NULL")
        .bind(round_date)
        .fetch_all(&mut *tx)
        .await?;
    let Some(odds_x100) = price(&book) else {
        return Ok(Placed::Moved);
    };
    // The cap is on all of the account's bets in the round together: many
    // overlapping bets that would all win at once share one cap.
    let room = limits.max_profit - open_profit(&mut tx, account_id, round_date).await?;
    if crate::odds::payout(stake, odds_x100, None) - stake > room {
        return Ok(Placed::OverRoundCap);
    }
    let now = ts(Utc::now());
    let inserted: Option<(i64,)> = sqlx::query_as(
        "INSERT INTO bets (account_id, round_date, market, stake, odds_x100, placed_at, max_profit)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(account_id, round_date, market) DO NOTHING
         RETURNING id",
    )
    .bind(account_id)
    .bind(round_date)
    .bind(market)
    .bind(stake)
    .bind(odds_x100)
    .bind(&now)
    .bind(limits.max_profit)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((bet_id,)) = inserted else {
        return Ok(Placed::Already);
    };
    sqlx::query(
        "INSERT INTO ledger (account_id, round_date, kind, amount, ref, created_at)
         VALUES (?1, ?2, 'bet', ?3, ?4, ?5)",
    )
    .bind(account_id)
    .bind(round_date)
    .bind(-stake)
    .bind(format!("bet:{bet_id}"))
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Placed::In)
}

/// Settles a round's open bets. `outcome` is the round's winning and most picked
/// numbers for a round that was played, and `None` for one that cannot be judged
/// (nobody played, or it was called off): those bets are void and refunded.
async fn settle_bets(
    conn: &mut sqlx::SqliteConnection,
    round_date: &str,
    outcome: Option<crate::odds::Outcome>,
    now: &str,
) -> Result<()> {
    let Some(outcome) = outcome else {
        return void_bets(conn, None, round_date, "called off").await;
    };
    let open: Vec<(i64, i64, String, i64, i64, Option<i64>)> = sqlx::query_as(
        "SELECT id, account_id, market, stake, odds_x100, max_profit FROM bets
         WHERE round_date = ?1 AND result IS NULL",
    )
    .bind(round_date)
    .fetch_all(&mut *conn)
    .await?;
    for (id, account, market, stake, odds, cap) in open {
        let won = crate::odds::Market::parse(&market).is_some_and(|m| crate::odds::outcome(m, outcome));
        let payout = if won { crate::odds::payout(stake, odds, cap) } else { 0 };
        sqlx::query("UPDATE bets SET result = ?1, payout = ?2, settled_at = ?3 WHERE id = ?4")
            .bind(if won { "won" } else { "lost" })
            .bind(payout)
            .bind(now)
            .bind(id)
            .execute(&mut *conn)
            .await?;
        if won {
            sqlx::query(
                "INSERT INTO ledger (account_id, round_date, kind, amount, ref, created_at)
                 VALUES (?1, ?2, 'bet_win', ?3, ?4, ?5)
                 ON CONFLICT(account_id, kind, ref) DO NOTHING",
            )
            .bind(account)
            .bind(round_date)
            .bind(payout)
            .bind(format!("bet:{id}"))
            .bind(now)
            .execute(&mut *conn)
            .await?;
        }
    }
    Ok(())
}

/// Calls off the open bets of a round - one account's, or everyone's - and
/// gives every stake back.
async fn void_bets(
    conn: &mut sqlx::SqliteConnection,
    account_id: Option<i64>,
    round_date: &str,
    why: &str,
) -> Result<()> {
    let now = ts(Utc::now());
    let open: Vec<(i64, i64, i64)> = sqlx::query_as(
        "SELECT id, account_id, stake FROM bets
         WHERE round_date = ?1 AND result IS NULL AND (?2 IS NULL OR account_id = ?2)",
    )
    .bind(round_date)
    .bind(account_id)
    .fetch_all(&mut *conn)
    .await?;
    for (id, account, stake) in open {
        sqlx::query("UPDATE bets SET result = 'void', payout = ?1, settled_at = ?2 WHERE id = ?3")
            .bind(stake)
            .bind(&now)
            .bind(id)
            .execute(&mut *conn)
            .await?;
        sqlx::query(
            "INSERT INTO ledger (account_id, round_date, kind, amount, ref, note, created_at)
             VALUES (?1, ?2, 'refund', ?3, ?4, ?5, ?6)
             ON CONFLICT(account_id, kind, ref) DO NOTHING",
        )
        .bind(account)
        .bind(round_date)
        .bind(stake)
        .bind(format!("bet:{id}"))
        .bind(format!("bet {why}"))
        .bind(&now)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// An account's bets: on one round, or (with `since`) every round from
/// that date on, newest first.
pub async fn bets_of(db: &Db, account_id: i64, since: &str) -> Result<Vec<Bet>> {
    let rows = sqlx::query_as::<_, Bet>(
        "SELECT id, round_date, market, stake, odds_x100, result, payout, max_profit FROM bets
         WHERE account_id = ?1 AND round_date >= ?2
         ORDER BY round_date DESC, id DESC",
    )
    .bind(account_id)
    .bind(since)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// The open bets on a round, (market, stake, account): what moves its odds.
pub async fn book(db: &Db, round_date: &str) -> Result<Vec<(String, i64, i64)>> {
    Ok(sqlx::query_as(&format!(
        "SELECT b.market, b.stake, b.account_id FROM bets b JOIN accounts a ON a.id = b.account_id
         WHERE b.round_date = ?1 AND b.result IS NULL AND NOT (a.username {TEST_ACCOUNTS})"
    ))
        .bind(round_date)
        .fetch_all(db)
        .await?)
}

/// One line of an account's points history.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct LedgerRow {
    pub round_date: String,
    pub kind: String,
    pub amount: i64,
    pub note: Option<String>,
    pub created_at: String,
}

pub async fn ledger_rows(db: &Db, account_id: i64, limit: i64) -> Result<Vec<LedgerRow>> {
    let rows = sqlx::query_as::<_, LedgerRow>(
        "SELECT round_date, kind, amount, note, created_at FROM ledger
         WHERE account_id = ?1 ORDER BY id DESC LIMIT ?2",
    )
    .bind(account_id)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// A row of the points leaderboard.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct PointsRow {
    pub id: i64,
    pub username: String,
    pub points: i64,
}

/// Balances, highest first; or, with `since`, what each account gained from
/// playing since then - grants, catch-up and the sign-up bonus left out, so it is not
/// a measure of having turned up.
pub async fn points_board(db: &Db, since: Option<&str>, limit: i64) -> Result<Vec<PointsRow>> {
    let sql = match since {
        None => format!("SELECT a.id, a.username, SUM(l.amount) AS points
                 FROM ledger l JOIN accounts a ON a.id = l.account_id
                 WHERE a.deleted_at IS NULL AND a.banned_at IS NULL AND NOT (a.username {TEST_ACCOUNTS})
                 GROUP BY a.id ORDER BY points DESC, a.username LIMIT ?2"),
        Some(_) => format!("SELECT a.id, a.username, SUM(l.amount) AS points
                 FROM ledger l JOIN accounts a ON a.id = l.account_id
                 WHERE a.deleted_at IS NULL AND a.banned_at IS NULL AND NOT (a.username {TEST_ACCOUNTS})
                   AND l.created_at >= ?1 AND l.kind NOT IN ('grant', 'signup', 'catchup')
                 GROUP BY a.id HAVING points != 0 ORDER BY points DESC, a.username LIMIT ?2"),
    };
    let rows = sqlx::query_as::<_, PointsRow>(&sql)
        .bind(since.unwrap_or(""))
        .bind(limit)
        .fetch_all(db)
        .await?;
    Ok(rows)
}

// -------------------------------------------------------------- payouts// -------------------------------------------------------------- payouts

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
        "{}
         SELECT g.user_id
         FROM winners w
         JOIN guesses g ON g.round_date = w.round_date AND g.value = w.value
                        AND g.participates = 1
         WHERE w.round_date = ?2 AND g.user_id > 0
           AND NOT EXISTS (SELECT 1 FROM payouts p WHERE p.round_date = w.round_date)",
        winner_cte(Pool::Vienna)
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

    #[test]
    fn clockwork_is_the_same_time_of_day_on_enough_days() {
        let at = |h: i64, m: i64, s: i64| h * 3600 + m * 60 + s;
        let script = [at(9, 0, 5), at(9, 0, 40), at(9, 1, 2), at(9, 0, 15), at(9, 1, 30), at(18, 0, 0)];
        assert_eq!(super::clockwork(&script, 120, 5), Some(at(9, 0, 5)));
        let person = [at(8, 10, 0), at(12, 40, 0), at(9, 5, 0), at(21, 0, 0), at(13, 13, 0), at(9, 30, 0)];
        assert_eq!(super::clockwork(&person, 120, 5), None);
        // Around midnight it wraps.
        let late = [at(23, 59, 30), at(0, 0, 10), at(23, 59, 50), at(0, 0, 40), at(0, 1, 0)];
        assert_eq!(super::clockwork(&late, 120, 5), Some(at(23, 59, 30)));
    }
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
            insert_guess(db, Pool::Vienna, round, *user_id, *value, true)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn the_lowest_unique_number_wins_not_the_lowest_number() {
        let t = temp_db().await;
        // Two people grabbed 1, so it is burned. 2 is the lowest survivor.
        seed(&t.db, "2026-09-05", &[(1, 1), (2, 1), (3, 2), (4, 3)]).await;

        let r = round_summary(&t.db, Pool::Vienna, "2026-09-06", "2026-09-05")
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

        let r = round_summary(&t.db, Pool::Vienna, "2026-09-06", "2026-09-05")
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

        let r = round_summary(&t.db, Pool::Vienna, "2026-09-06", "2026-09-05")
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
        let earlier = rounds_before(&t.db, Pool::Vienna, open, "2026-09-05", 10).await.unwrap();
        let dates: Vec<&str> = earlier.iter().map(|r| r.round_date.as_str()).collect();
        assert_eq!(dates, ["2026-09-04", "2026-09-03"], "newest first, and not itself");
        assert_eq!(earlier[0].winning_value, Some(2));
        assert_eq!(earlier[1].winning_value, None, "every number collided that day");

        // The limit cuts the oldest rounds, not the nearest ones.
        let one = rounds_before(&t.db, Pool::Vienna, open, "2026-09-05", 1).await.unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].round_date, "2026-09-04");

        assert_eq!(
            round_after(&t.db, Pool::Vienna, open, "2026-09-04").await.unwrap().as_deref(),
            Some("2026-09-05")
        );
        // The open round is not a round to link to, and the first day has
        // nothing before it.
        assert_eq!(round_after(&t.db, Pool::Vienna, open, "2026-09-05").await.unwrap(), None);
        assert!(rounds_before(&t.db, Pool::Vienna, open, "2026-09-03", 10).await.unwrap().is_empty());

        assert_eq!(closed_round_count(&t.db, Pool::Vienna, open).await.unwrap(), 3);
    }

    #[tokio::test]
    async fn the_open_round_is_never_scored() {
        let t = temp_db().await;
        seed(&t.db, "2026-09-06", &[(1, 1), (2, 2)]).await;

        assert!(round_summary(&t.db, Pool::Vienna, "2026-09-06", "2026-09-06")
            .await
            .unwrap()
            .is_none());
        assert!(closed_rounds(&t.db, Pool::Vienna, "2026-09-06", 10)
            .await
            .unwrap()
            .is_empty());
        // ...and it is still visible as an in-progress headcount.
        assert_eq!(guess_count(&t.db, Pool::Vienna, "2026-09-06").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn a_guess_is_final_and_a_second_one_is_refused() {
        let t = temp_db().await;
        seed(&t.db, "2026-09-05", &[(1, 50)]).await;

        // The second attempt reports that nothing was written...
        assert!(!insert_guess(&t.db, Pool::Vienna, "2026-09-05", 1, 3, true).await.unwrap());
        // ...and the original number is untouched.
        assert_eq!(my_guess(&t.db, Pool::Vienna, "2026-09-05", 1).await.unwrap(), Some(50));
        assert_eq!(guess_count(&t.db, Pool::Vienna, "2026-09-05").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn the_same_player_may_guess_again_in_a_later_round() {
        let t = temp_db().await;
        seed(&t.db, "2026-09-05", &[(1, 50)]).await;

        assert!(insert_guess(&t.db, Pool::Vienna, "2026-09-06", 1, 3, true).await.unwrap());
        assert_eq!(my_guess(&t.db, Pool::Vienna, "2026-09-05", 1).await.unwrap(), Some(50));
        assert_eq!(my_guess(&t.db, Pool::Vienna, "2026-09-06", 1).await.unwrap(), Some(3));
    }

    #[tokio::test]
    async fn history_is_newest_first_and_the_leaderboard_tallies_wins() {
        let t = temp_db().await;
        seed(&t.db, "2026-09-03", &[(1, 1), (2, 2)]).await; // p1 wins
        seed(&t.db, "2026-09-04", &[(1, 4), (2, 4)]).await; // nobody wins
        seed(&t.db, "2026-09-05", &[(1, 8), (2, 3)]).await; // p2 wins
        seed(&t.db, "2026-09-06", &[(1, 1)]).await; // still open

        let rounds = closed_rounds(&t.db, Pool::Vienna, "2026-09-06", 10).await.unwrap();
        let dates: Vec<&str> = rounds.iter().map(|r| r.round_date.as_str()).collect();
        assert_eq!(dates, ["2026-09-05", "2026-09-04", "2026-09-03"]);
        assert_eq!(rounds[1].winning_value, None);

        let board = leaderboard(&t.db, Pool::Vienna, "2026-09-06", 10).await.unwrap();
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
        insert_guess(&t.db, Pool::Vienna, "2026-09-05", -1, 2, false).await.unwrap();

        let tallies = round_tallies(&t.db, Pool::Vienna, "2026-09-05").await.unwrap();
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

        let rows = range_tallies(&t.db, Pool::Vienna, "2026-09-05", "2026-09-07").await.unwrap();
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
        assert_eq!(insert_demo(&t.db, Pool::Vienna, &guesses).await.unwrap(), 3);
        // Running it again adds nothing: those bots already guessed those days.
        assert_eq!(insert_demo(&t.db, Pool::Vienna, &guesses).await.unwrap(), 0);
        assert_eq!(demo_counts(&t.db).await.unwrap(), (demo::BOTS as i64, 3));

        // Bots stay out of the sign-in-as list.
        let listed: Vec<String> = demo_users(&t.db).await.unwrap().into_iter().map(|u| u.login).collect();
        assert_eq!(listed, ["alice"]);

        remove_demo(&t.db).await.unwrap();
        assert_eq!(demo_counts(&t.db).await.unwrap(), (0, 0));
        // The real player's guess and the hand-made stand-in survive.
        assert_eq!(guess_count(&t.db, Pool::Vienna, "2026-09-05").await.unwrap(), 1);
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
        assert!(insert_guess(&t.db, Pool::Vienna, "2026-09-05", -1, 1, false)
            .await
            .unwrap());

        let r = round_summary(&t.db, Pool::Vienna, "2026-09-06", "2026-09-05")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.winning_value, Some(5), "a ghost must not win");
        assert_eq!(r.total, 2, "a ghost must not be counted");
        assert_eq!(guess_count(&t.db, Pool::Vienna, "2026-09-05").await.unwrap(), 2);
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
        assert!(insert_guess(&t.db, Pool::Vienna, "2026-09-05", -1, 3, false)
            .await
            .unwrap());

        let r = round_summary(&t.db, Pool::Vienna, "2026-09-06", "2026-09-05")
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
        insert_guess(&t.db, Pool::Vienna, "2026-09-07", -1, 1, false)
            .await
            .unwrap();

        let rows = round_guesses(&t.db, Pool::Vienna, "2026-09-07").await.unwrap();
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
        let flow = OAuthState {
            provider: "github".into(),
            link_account_id: Some(4),
            pkce_verifier: Some("v".into()),
        };
        store_oauth_state(&t.db, "abc", &flow).await.unwrap();

        assert_eq!(consume_oauth_state(&t.db, "abc").await.unwrap(), Some(flow));
        assert_eq!(consume_oauth_state(&t.db, "abc").await.unwrap(), None);
        assert_eq!(consume_oauth_state(&t.db, "never-issued").await.unwrap(), None);

        // An expired one is refused, and gone all the same.
        sqlx::query("INSERT INTO oauth_states (state, created_at, expires_at) VALUES ('old', 'x', '2000-01-01T00:00:00Z')")
            .execute(&t.db)
            .await
            .unwrap();
        assert_eq!(consume_oauth_state(&t.db, "old").await.unwrap(), None);
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM oauth_states").await, 0);
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
    async fn a_guess_forgets_its_network_after_ninety_days() {
        let t = temp_db().await;
        let a = signed_in(&t.db, 7, "ann", "a1").await.id;
        let today = Utc::now().date_naive();
        let day = |back: i64| (today - chrono::Duration::days(back)).format("%Y-%m-%d").to_string();
        for back in [GUESS_IP_HASH_DAYS + 1, GUESS_IP_HASH_DAYS - 1] {
            insert_guess(&t.db, Pool::Global, &day(back), a, 3, true).await.unwrap();
            set_guess_ip_hash(&t.db, &day(back), a, "abc").await.unwrap();
        }
        purge_expired(&t.db).await.unwrap();
        let kept: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT round_date, ip_hash FROM global_guesses ORDER BY round_date").fetch_all(&t.db).await.unwrap();
        assert_eq!(kept, vec![(day(GUESS_IP_HASH_DAYS + 1), None), (day(GUESS_IP_HASH_DAYS - 1), Some("abc".into()))]);
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
        insert_demo(&t.db, Pool::Vienna, &guesses).await.unwrap();
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

    // ------------------------------------------------------- the global pool

    /// A verified account with no 42 player behind it.
    async fn global_player(db: &Db, username: &str) -> i64 {
        let id = plain_account(db, username).await;
        sqlx::query("UPDATE accounts SET verified_at = created_at WHERE id = ?1")
            .bind(id)
            .execute(db)
            .await
            .unwrap();
        id
    }

    #[tokio::test]
    async fn the_global_pool_is_won_the_same_way_under_usernames() {
        let t = temp_db().await;
        let ann = global_player(&t.db, "ann").await;
        let bob = global_player(&t.db, "bob").await;
        let student = signed_in(&t.db, 7, "megardes", "tok").await;
        for (who, value) in [(ann, 1), (bob, 1), (student.id, 2)] {
            assert!(insert_guess(&t.db, Pool::Global, "2026-09-05", who, value, true).await.unwrap());
        }

        let r = round_summary(&t.db, Pool::Global, "2026-09-06", "2026-09-05")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.winning_value, Some(2));
        assert_eq!(r.total, 3);
        // Named by username alone: nothing says this one is a 42 student.
        assert_eq!(r.winner_login.as_deref(), Some("megardes"));
        assert_eq!(r.winner_name.as_deref(), Some("megardes"));

        let board = leaderboard(&t.db, Pool::Global, "2026-09-06", 10).await.unwrap();
        assert_eq!(board.len(), 1);
        assert_eq!((board[0].login.as_str(), board[0].wins), ("megardes", 1));
    }

    #[tokio::test]
    async fn the_two_pools_never_see_each_others_guesses() {
        let t = temp_db().await;
        let student = signed_in(&t.db, 7, "megardes", "tok").await;
        let ann = global_player(&t.db, "ann").await;
        upsert_user(&t.db, 8, "other", "Other", None).await.unwrap();

        // The same student plays both pools on the same day, with different numbers.
        insert_guess(&t.db, Pool::Vienna, "2026-09-05", 7, 3, true).await.unwrap();
        insert_guess(&t.db, Pool::Vienna, "2026-09-05", 8, 5, true).await.unwrap();
        insert_guess(&t.db, Pool::Global, "2026-09-05", student.id, 5, true).await.unwrap();
        insert_guess(&t.db, Pool::Global, "2026-09-05", ann, 3, true).await.unwrap();

        // Each pool is won on its own numbers: a 3 in one pool does not burn
        // the 3 in the other.
        let vienna = round_summary(&t.db, Pool::Vienna, "2026-09-06", "2026-09-05").await.unwrap().unwrap();
        let global = round_summary(&t.db, Pool::Global, "2026-09-06", "2026-09-05").await.unwrap().unwrap();
        assert_eq!((vienna.winning_value, vienna.winner_login.as_deref()), (Some(3), Some("megardes")));
        assert_eq!((global.winning_value, global.winner_login.as_deref()), (Some(3), Some("ann")));

        assert_eq!(my_guess(&t.db, Pool::Vienna, "2026-09-05", 7).await.unwrap(), Some(3));
        assert_eq!(my_guess(&t.db, Pool::Global, "2026-09-05", student.id).await.unwrap(), Some(5));
        assert_eq!(guess_count(&t.db, Pool::Vienna, "2026-09-06").await.unwrap(), 0);
        assert_eq!(
            range_tallies(&t.db, Pool::Global, "", "2026-09-06").await.unwrap().len(),
            2
        );
        // Coalition points only ever look at the 42 pool.
        assert_eq!(
            unpaid_round(&t.db, "2026-09-06", "2026-09-05").await.unwrap().map(|p| p.winner_id),
            Some(7)
        );
    }

    #[tokio::test]
    async fn a_global_guess_is_final_too() {
        let t = temp_db().await;
        let ann = global_player(&t.db, "ann").await;
        assert!(insert_guess(&t.db, Pool::Global, "2026-09-05", ann, 50, true).await.unwrap());
        assert!(!insert_guess(&t.db, Pool::Global, "2026-09-05", ann, 3, true).await.unwrap());
        assert_eq!(my_guess(&t.db, Pool::Global, "2026-09-05", ann).await.unwrap(), Some(50));
    }

    #[tokio::test]
    async fn global_ghosts_and_clearing_stay_in_their_pool() {
        let t = temp_db().await;
        let ann = global_player(&t.db, "ann").await;
        let ghost = global_player(&t.db, "ghost").await;
        upsert_user(&t.db, 7, "megardes", "M", None).await.unwrap();
        insert_guess(&t.db, Pool::Global, "2026-09-05", ann, 5, true).await.unwrap();
        insert_guess(&t.db, Pool::Global, "2026-09-05", ghost, 1, false).await.unwrap();
        insert_guess(&t.db, Pool::Vienna, "2026-09-05", 7, 9, true).await.unwrap();

        let r = round_summary(&t.db, Pool::Global, "2026-09-06", "2026-09-05").await.unwrap().unwrap();
        assert_eq!((r.winning_value, r.total), (Some(5), 1), "a ghost neither wins nor counts");
        assert_eq!(round_guesses(&t.db, Pool::Global, "2026-09-05").await.unwrap().len(), 2);

        assert_eq!(clear_round(&t.db, Pool::Global, "2026-09-05").await.unwrap(), 2);
        assert_eq!(guess_count(&t.db, Pool::Vienna, "2026-09-05").await.unwrap(), 1, "42 pool untouched");
    }

    #[tokio::test]
    async fn global_demo_history_goes_in_and_comes_back_out_cleanly() {
        let t = temp_db().await;
        let ann = global_player(&t.db, "ann").await;
        insert_guess(&t.db, Pool::Global, "2026-09-05", ann, 4, true).await.unwrap();
        let guesses = vec![
            DemoGuess { round_date: "2026-09-05".into(), bot: 0, value: 3 },
            DemoGuess { round_date: "2026-09-05".into(), bot: 1, value: 7 },
        ];
        assert_eq!(insert_demo(&t.db, Pool::Global, &guesses).await.unwrap(), 2);
        assert_eq!(insert_demo(&t.db, Pool::Global, &guesses).await.unwrap(), 0);
        assert_eq!(insert_demo(&t.db, Pool::Vienna, &guesses).await.unwrap(), 2);
        assert_eq!(demo_counts(&t.db).await.unwrap(), (demo::BOTS as i64, 4));

        let r = round_summary(&t.db, Pool::Global, "2026-09-06", "2026-09-05").await.unwrap().unwrap();
        assert_eq!((r.winning_value, r.winner_login.as_deref()), (Some(3), Some("bot-001")));

        remove_demo(&t.db).await.unwrap();
        assert_eq!(demo_counts(&t.db).await.unwrap(), (0, 0));
        // The real player, their guess and their account are left alone.
        assert_eq!(guess_count(&t.db, Pool::Global, "2026-09-05").await.unwrap(), 1);
        assert_eq!(count(&t.db, "SELECT COUNT(*) FROM accounts").await, 1);
    }

    #[tokio::test]
    async fn a_42_sign_in_verifies_the_account() {
        let t = temp_db().await;
        let account = signed_in(&t.db, 7, "megardes", "tok").await;
        assert!(account.verified);
        assert!(viewer_for_session(&t.db, "tok").await.unwrap().unwrap().account.verified);
        let plain = plain_account(&t.db, "stranger").await;
        create_session(&t.db, "p", plain, 30).await.unwrap();
        assert!(!viewer_for_session(&t.db, "p").await.unwrap().unwrap().account.verified);
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
        let before_rounds = closed_rounds(&old, Pool::Vienna, open, 100).await.unwrap();
        let before_board = leaderboard(&old, Pool::Vienna, open, 100).await.unwrap();
        let before_tallies = range_tallies(&old, Pool::Vienna, "", open).await.unwrap();
        old.close().await;

        t.db = connect(&format!("sqlite://{}", t.path.display())).await.unwrap();
        let db = &t.db;

        // ...is exactly what it says after.
        let fmt = |v: &dyn std::fmt::Debug| format!("{v:?}");
        assert_eq!(fmt(&closed_rounds(db, Pool::Vienna, open, 100).await.unwrap()), fmt(&before_rounds));
        assert_eq!(fmt(&leaderboard(db, Pool::Vienna, open, 100).await.unwrap()), fmt(&before_board));
        assert_eq!(range_tallies(db, Pool::Vienna, "", open).await.unwrap(), before_tallies);
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
        assert_eq!(
            consume_oauth_state(db, "pending").await.unwrap().map(|f| f.provider),
            Some("42".to_string())
        );

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
        let before_rounds = format!("{:?}", closed_rounds(&copy, Pool::Vienna, &open, i64::MAX).await.unwrap());
        let before_board = format!("{:?}", leaderboard(&copy, Pool::Vienna, &open, i64::MAX).await.unwrap());
        let before_tallies = range_tallies(&copy, Pool::Vienna, "", &open).await.unwrap();
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
        assert_eq!(format!("{:?}", closed_rounds(&db, Pool::Vienna, &open, i64::MAX).await.unwrap()), before_rounds);
        assert_eq!(format!("{:?}", leaderboard(&db, Pool::Vienna, &open, i64::MAX).await.unwrap()), before_board);
        assert_eq!(range_tallies(&db, Pool::Vienna, "", &open).await.unwrap(), before_tallies);
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
        insert_guess(&t.db, Pool::Vienna, "2026-09-05", 14, 9, false).await.unwrap();
        let bot = create_test_user(&t.db, "bot-001").await.unwrap();
        insert_guess(&t.db, Pool::Vienna, "2026-09-05", bot.id, 7, true).await.unwrap();

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
        insert_guess(&t.db, Pool::Vienna, "2026-09-03", bot.id, 1, true).await.unwrap();
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
