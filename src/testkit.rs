//! Helpers for tests that drive the real handlers: an app on a throwaway
//! database, session cookies, and a look at what came back.

use std::sync::{Arc, Mutex};

use axum::http::header::{LOCATION, SET_COOKIE};
use axum::response::Response;
use axum_extra::extract::cookie::{Cookie, CookieJar};

use crate::app::AppState;
use crate::auth;
use crate::config::Config;
use crate::db::{self, test_support::{temp_db, TempDb}};
use crate::mail::{Mail, Mailer};

pub fn config(admins: &[&str]) -> Config {
    Config {
        admin_logins: admins.iter().map(|a| a.to_string()).collect(),
        allowed_campus_ids: vec![53],
        public_url: "http://test".to_string(),
        mail_daily_limit: 90,
        ..Config::default()
    }
}

/// An app on a fresh database. Mail is kept in memory instead of sent.
pub async fn app(admins: &[&str]) -> (TempDb, AppState) {
    let (t, state, _) = app_with(config(admins)).await;
    (t, state)
}

/// As [`app`], with a config of the test's own and the mails it sends.
pub async fn app_with(cfg: Config) -> (TempDb, AppState, Arc<Mutex<Vec<Mail>>>) {
    let t = temp_db().await;
    let mut state = AppState::new(t.db.clone(), Arc::new(cfg), reqwest::Client::new());
    let (mailer, sent) = Mailer::memory();
    state.mailer = mailer;
    (t, state, sent)
}

pub fn jar(token: &str) -> CookieJar {
    CookieJar::new().add(Cookie::new(auth::SESSION_COOKIE, token.to_string()))
}

/// A 42 student with a session. Their account id and intra id differ on
/// purpose, so a mix-up between the two cannot pass unnoticed.
pub async fn student(db: &db::Db, intra_id: i64, login: &str, token: &str) {
    db::upsert_user(db, intra_id, login, login, None).await.unwrap();
    let account = db::account_for_intra(db, intra_id, login).await.unwrap();
    assert_ne!(account.id, intra_id);
    db::create_session(db, token, account.id, 30).await.unwrap();
}

/// Signed in, but with no 42 account behind it, and not verified.
pub async fn stranger(db: &db::Db, username: &str, token: &str) {
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO accounts (username, created_at, last_seen_at)
         VALUES (?1, 'x', 'x') RETURNING id",
    )
    .bind(username)
    .fetch_one(db)
    .await
    .unwrap();
    db::create_session(db, token, id, 30).await.unwrap();
}

pub fn location(resp: &Response) -> &str {
    resp.headers().get(LOCATION).and_then(|v| v.to_str().ok()).unwrap_or("")
}

/// The value a response sets for a cookie, if it sets one.
pub fn set_cookie(resp: &Response, name: &str) -> Option<String> {
    resp.headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|v| Cookie::parse(v.to_str().ok()?.to_string()).ok())
        .find(|c| c.name() == name)
        .map(|c| c.value().to_string())
}

pub async fn body(resp: Response) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}
