//! `/mod`: keeping the global pool honest. Moderators are named by 42 login
//! in MODERATORS and work on the live site, unlike `/admin`, which only
//! exists on a test instance. They can ban and unban accounts, and see
//! groups of accounts that may be one person playing several.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;

use crate::app::{error_page, AppError, AppState};
use crate::credentials::normalize_username;
use crate::db::{self, Pool, Viewer};
use crate::handlers::current_viewer;
use crate::round::Round;
use crate::templates::{self, ImprintTemplate, ModTemplate, Notice, PrivacyTemplate, UserView};

/// Accounts in a group before it is worth a moderator's look.
const CLUSTER_MIN: i64 = 3;

/// The signed-in moderator, or `None`, which callers answer with a 404 so
/// the page gives no sign that it exists.
async fn moderator(state: &AppState, jar: &CookieJar) -> Result<Option<Viewer>, AppError> {
    let viewer = current_viewer(state, jar).await?;
    Ok(viewer.filter(|v| v.intra.as_ref().is_some_and(|u| state.cfg.is_moderator(&u.login))))
}

fn gone() -> Response {
    error_page(StatusCode::NOT_FOUND, "No such page.")
}

#[derive(Deserialize, Default)]
pub struct ModQuery {
    pub msg: Option<String>,
}

async fn page(state: &AppState, viewer: Viewer, notice: Option<Notice>, status: StatusCode) -> Result<Response, AppError> {
    let mut resp = templates::render(&ModTemplate {
        pool: Pool::Global,
        user: Some(UserView::from(viewer)),
        test_mode: state.cfg.test_mode(),
        impersonating: false,
        notice,
        clusters: db::clusters(&state.db, CLUSTER_MIN).await?,
        banned: db::banned_accounts(&state.db).await?,
    });
    *resp.status_mut() = status;
    Ok(resp)
}

pub async fn moderation(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(q): Query<ModQuery>,
) -> Result<Response, AppError> {
    let Some(viewer) = moderator(&state, &jar).await? else {
        return Ok(gone());
    };
    let notice = match q.msg.as_deref() {
        Some("banned") => Some(Notice::ok("Banned.")),
        Some("unbanned") => Some(Notice::ok("Unbanned.")),
        _ => None,
    };
    page(&state, viewer, notice, StatusCode::OK).await
}

#[derive(Deserialize)]
pub struct BanForm {
    pub username: String,
    #[serde(default)]
    pub reason: String,
}

pub async fn ban(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<BanForm>,
) -> Result<Response, AppError> {
    let Some(viewer) = moderator(&state, &jar).await? else {
        return Ok(gone());
    };
    let refuse = |why: &'static str| page(&state, viewer.clone(), Some(Notice::error(why)), StatusCode::UNPROCESSABLE_ENTITY);
    let reason = form.reason.trim();
    if reason.is_empty() {
        return refuse("Say why: the reason is kept with the ban.").await;
    }
    let Some(target) = db::mod_account(&state.db, &normalize_username(&form.username)).await? else {
        return refuse("No account by that name.").await;
    };
    if target.id == viewer.account.id {
        return refuse("You can't ban yourself.").await;
    }
    let open_round = Round::current(state.now()).key();
    db::ban_account(&state.db, target.id, reason, &open_round).await?;
    let by = viewer.intra.as_ref().map(|u| u.login.as_str()).unwrap_or("?");
    tracing::info!(moderator = by, account = target.id, username = %target.username, reason, "banned");
    Ok(Redirect::to("/mod?msg=banned").into_response())
}

#[derive(Deserialize)]
pub struct UnbanForm {
    pub username: String,
}

pub async fn unban(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<UnbanForm>,
) -> Result<Response, AppError> {
    let Some(viewer) = moderator(&state, &jar).await? else {
        return Ok(gone());
    };
    if let Some(target) = db::mod_account(&state.db, &normalize_username(&form.username)).await? {
        db::unban_account(&state.db, target.id).await?;
        let by = viewer.intra.as_ref().map(|u| u.login.as_str()).unwrap_or("?");
        tracing::info!(moderator = by, account = target.id, username = %target.username, "unbanned");
    }
    Ok(Redirect::to("/mod?msg=unbanned").into_response())
}

// --------------------------------------------------------- legal pages

pub async fn privacy(State(state): State<AppState>, jar: CookieJar) -> Result<Response, AppError> {
    let viewer = current_viewer(&state, &jar).await?;
    Ok(templates::render(&PrivacyTemplate {
        pool: Pool::Global,
        user: viewer.map(UserView::from),
        test_mode: state.cfg.test_mode(),
        impersonating: false,
    }))
}

pub async fn imprint(State(state): State<AppState>, jar: CookieJar) -> Result<Response, AppError> {
    let viewer = current_viewer(&state, &jar).await?;
    Ok(templates::render(&ImprintTemplate {
        pool: Pool::Global,
        user: viewer.map(UserView::from),
        test_mode: state.cfg.test_mode(),
        impersonating: false,
        name: state.cfg.imprint_name.clone(),
        address: state.cfg.imprint_address.clone(),
        email: state.cfg.imprint_email.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use axum::extract::{Query, State};
    use axum::http::StatusCode;
    use axum::Form;

    use super::*;
    use crate::testkit::{app_with, body, config, jar, location, stranger, student};

    async fn setup(mods: &[&str]) -> (crate::db::test_support::TempDb, AppState) {
        let mut cfg = config(&[]);
        cfg.moderators = mods.iter().map(|m| m.to_string()).collect();
        let (t, state, _) = app_with(cfg).await;
        (t, state)
    }

    async fn status_of(state: &AppState, token: &str) -> StatusCode {
        moderation(State(state.clone()), jar(token), Query(ModQuery::default())).await.unwrap().status()
    }

    #[tokio::test]
    async fn only_named_42_logins_moderate() {
        let (t, state) = setup(&["megardes"]).await;
        student(&t.db, 1001, "megardes", "mod").await;
        student(&t.db, 1002, "other", "student").await;
        // Took the moderator's name as a username, without being them.
        stranger(&t.db, "megardes-42", "x").await;
        sqlx::query("UPDATE accounts SET username = 'imposter' WHERE username = 'megardes'").execute(&t.db).await.unwrap();
        stranger(&t.db, "megardes", "fake").await;

        assert_eq!(status_of(&state, "mod").await, StatusCode::OK);
        assert_eq!(status_of(&state, "student").await, StatusCode::NOT_FOUND);
        assert_eq!(status_of(&state, "fake").await, StatusCode::NOT_FOUND);
        let resp = moderation(State(state.clone()), axum_extra::extract::cookie::CookieJar::new(), Query(ModQuery::default())).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // And nobody, when MODERATORS is empty on a live site.
        let (t2, nobody) = setup(&[]).await;
        student(&t2.db, 1001, "megardes", "mod").await;
        assert_eq!(status_of(&nobody, "mod").await, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_ban_signs_out_voids_today_and_leaves_closed_rounds() {
        let (t, state) = setup(&["megardes"]).await;
        student(&t.db, 1001, "megardes", "mod").await;
        stranger(&t.db, "cheat", "c").await;
        let cheat = db::viewer_for_session(&t.db, "c").await.unwrap().unwrap().account.id;
        let today = Round::current(state.now()).key();
        let yesterday = Round::current(state.now()).previous_date().format("%Y-%m-%d").to_string();
        db::insert_guess(&t.db, Pool::Global, &today, cheat, 1, true).await.unwrap();
        db::insert_guess(&t.db, Pool::Global, &yesterday, cheat, 1, true).await.unwrap();

        let ban_form = |username: &str, reason: &str| Form(BanForm { username: username.into(), reason: reason.into() });
        let resp = ban(State(state.clone()), jar("mod"), ban_form("cheat", "")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "a reason is required");
        let resp = ban(State(state.clone()), jar("mod"), ban_form("nobody", "x")).await.unwrap();
        assert!(body(resp).await.contains("No account by that name"));
        let resp = ban(State(state.clone()), jar("mod"), ban_form("megardes", "x")).await.unwrap();
        assert!(body(resp).await.contains("ban yourself"));

        let resp = ban(State(state.clone()), jar("mod"), ban_form(" Cheat ", "blocking with alts")).await.unwrap();
        assert_eq!(location(&resp), "/mod?msg=banned");
        assert!(db::viewer_for_session(&t.db, "c").await.unwrap().is_none());
        assert_eq!(db::guess_count(&t.db, Pool::Global, &today).await.unwrap(), 0, "today's guess stops counting");
        let r = db::round_summary(&t.db, Pool::Global, &today, &yesterday).await.unwrap().unwrap();
        assert_eq!(r.winner_login.as_deref(), Some("cheat"), "closed rounds are not rewritten");

        let html = body(moderation(State(state.clone()), jar("mod"), Query(ModQuery::default())).await.unwrap()).await;
        assert!(html.contains("blocking with alts"));

        let resp = unban(State(state.clone()), jar("mod"), Form(UnbanForm { username: "cheat".into() })).await.unwrap();
        assert_eq!(location(&resp), "/mod?msg=unbanned");
        db::create_session(&t.db, "c2", cheat, 30).await.unwrap();
        assert!(db::viewer_for_session(&t.db, "c2").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn accounts_that_share_an_address_or_a_minute_are_grouped() {
        let (t, _state) = setup(&["megardes"]).await;
        let now = chrono::Utc::now();
        let at = |minutes: i64| (now - chrono::Duration::minutes(minutes)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        // Three from one address, spread over days; two from another.
        for (name, hash, ago) in [
            ("a1", "aaaa", 3000), ("a2", "aaaa", 6000), ("a3", "aaaa", 9000),
            ("b1", "bbbb", 12000), ("b2", "bbbb", 15000),
            // A burst of three within ten minutes, from different addresses.
            ("c1", "c1", 100), ("c2", "c2", 95), ("c3", "c3", 91),
        ] {
            sqlx::query("INSERT INTO accounts (username, created_at, last_seen_at, signup_ip_hash) VALUES (?1, ?2, ?2, ?3)")
                .bind(name).bind(at(ago)).bind(hash).execute(&t.db).await.unwrap();
        }
        // b1, b2 and a1 guessed from one place this week.
        let today = now.format("%Y-%m-%d").to_string();
        for name in ["b1", "b2", "a1"] {
            let (id,): (i64,) = sqlx::query_as("SELECT id FROM accounts WHERE username = ?1").bind(name).fetch_one(&t.db).await.unwrap();
            db::insert_guess(&t.db, Pool::Global, &today, id, 5, true).await.unwrap();
            db::set_guess_ip_hash(&t.db, &today, id, "dddd").await.unwrap();
        }

        let found = db::clusters(&t.db, 3).await.unwrap();
        let groups: Vec<Vec<&str>> = found.iter().map(|c| c.usernames.iter().map(String::as_str).collect()).collect();
        assert!(groups.contains(&vec!["a1", "a2", "a3"]), "{found:?}");
        assert!(groups.contains(&vec!["a1", "b1", "b2"]), "{found:?}");
        assert!(groups.contains(&vec!["c1", "c2", "c3"]), "{found:?}");
        assert_eq!(found.len(), 3, "two from one address is not a group: {found:?}");
    }

    #[tokio::test]
    async fn the_legal_pages_render() {
        let mut cfg = config(&[]);
        cfg.imprint_name = Some("Jane Doe".into());
        cfg.imprint_email = Some("hi@example.com".into());
        let (_t, state, _) = app_with(cfg).await;
        let html = body(privacy(State(state.clone()), axum_extra::extract::cookie::CookieJar::new()).await.unwrap()).await;
        assert!(html.contains("one cookie"));
        let html = body(imprint(State(state.clone()), axum_extra::extract::cookie::CookieJar::new()).await.unwrap()).await;
        assert!(html.contains("Jane Doe"));
        assert!(html.contains("mailto:hi@example.com"));
        assert!(html.contains("not given"), "the address was not set");
    }
}
