//! `/account`: the signed-in visitor's own page. Their username, the ways
//! they sign in (adding and removing them), their password, and deleting the
//! account.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use axum_extra::extract::cookie::CookieJar;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::app::{AppError, AppState};
use crate::auth;
use crate::credentials::{self, normalize_email, normalize_username, password_problem, username_problem};
use crate::db::{self, Pool, Unlinked, Viewer};
use crate::handlers::current_viewer;
use crate::limits;
use crate::oauth::Provider;
use crate::signin::send_verification;
use crate::templates::{self, AccountTemplate, LoginView, Notice, UserView};

/// A username can change once in this long, so a name cannot be worn
/// briefly to borrow someone's standing and then dropped.
const RENAME_EVERY: chrono::Duration = chrono::Duration::days(30);

#[derive(Deserialize, Default)]
pub struct AccountQuery {
    pub msg: Option<String>,
}

fn notice_for(code: Option<&str>) -> Option<Notice> {
    let (kind, text) = crate::copy::notice(code?)?;
    Some(if kind == "ok" { Notice::ok(text) } else { Notice::error(text) })
}

/// When the next rename is allowed, if not yet.
fn next_rename(changed_at: Option<&str>) -> Option<DateTime<Utc>> {
    let changed = DateTime::parse_from_rfc3339(changed_at?).ok()?.with_timezone(&Utc);
    let next = changed + RENAME_EVERY;
    (next > Utc::now()).then_some(next)
}

async fn page(state: &AppState, viewer: Viewer, notice: Option<Notice>, status: StatusCode) -> Result<Response, AppError> {
    let id = viewer.account.id;
    let logins = db::account_logins(&state.db, id).await?;
    let password = db::password_email(&state.db, id).await?;
    let next = next_rename(db::username_changed_at(&state.db, id).await?.as_deref());
    let plays_42 = viewer.account.intra_id.is_some();

    let mut views = Vec::new();
    for provider in [Provider::Intra, Provider::Github, Provider::Google] {
        let found = logins.iter().find(|l| l.provider == provider.key());
        let detail = match (provider, found) {
            (Provider::Intra, Some(_)) => viewer
                .intra
                .as_ref()
                .map_or_else(|| "global round only".to_string(), |u| u.login.clone()),
            (_, Some(l)) => l.email.clone().unwrap_or_else(|| "linked".to_string()),
            (_, None) => String::new(),
        };
        let (can_unlink, why_not) = match (provider, found) {
            (Provider::Intra, Some(_)) if plays_42 => (false, "your 42 Vienna round needs it"),
            (_, Some(_)) if logins.len() <= 1 => (false, "your only way in"),
            _ => (true, ""),
        };
        views.push(LoginView {
            key: provider.key(),
            name: provider.name(),
            linked: found.is_some(),
            detail,
            available: provider.enabled(state),
            can_unlink,
            why_not,
        });
    }
    let has_password = logins.iter().any(|l| l.provider == "email");
    if has_password {
        views.push(LoginView {
            key: "email",
            name: "email",
            linked: true,
            detail: password.as_ref().map(|p| p.0.clone()).unwrap_or_default(),
            available: true,
            can_unlink: logins.len() > 1,
            why_not: "your only way in",
        });
    }

    let username = viewer.account.username.clone();
    let page = AccountTemplate {
        pool: Pool::Global,
        test_mode: state.cfg.test_env(),
        impersonating: false,
        notice,
        account_id: id,
        username,
        plays_42,
        can_rename: next.is_none(),
        rename_hint: match next {
            Some(when) => format!("you can change it again on {}.", when.format("%-d %B %Y")),
            None => "3–20 characters: letters, digits, - and _. once a month.".to_string(),
        },
        logins: views,
        password_email: password.as_ref().map(|p| p.0.clone()),
        password_verified: password.as_ref().is_some_and(|p| p.1),
        email_signup: state.cfg.email_signup(),
        user: Some(UserView::from(viewer)),
    };
    let mut resp = templates::render(&page);
    *resp.status_mut() = status;
    Ok(resp)
}

pub async fn account(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(q): Query<AccountQuery>,
) -> Result<Response, AppError> {
    let Some(viewer) = current_viewer(&state, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    page(&state, viewer, notice_for(q.msg.as_deref()), StatusCode::OK).await
}

/// The signed-in viewer, or the redirect to sign in.
async fn signed_in(state: &AppState, jar: &CookieJar) -> Result<Result<Viewer, Response>, AppError> {
    Ok(match current_viewer(state, jar).await? {
        Some(v) => Ok(v),
        None => Err(Redirect::to("/login").into_response()),
    })
}

async fn refuse(state: &AppState, viewer: Viewer, why: impl Into<String>) -> Result<Response, AppError> {
    page(state, viewer, Some(Notice::error(why)), StatusCode::UNPROCESSABLE_ENTITY).await
}

#[derive(Deserialize)]
pub struct RenameForm {
    pub username: String,
}

pub async fn rename(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<RenameForm>,
) -> Result<Response, AppError> {
    let viewer = match signed_in(&state, &jar).await? {
        Ok(v) => v,
        Err(away) => return Ok(away),
    };
    let username = normalize_username(&form.username);
    if username == viewer.account.username {
        return Ok(Redirect::to("/account").into_response());
    }
    if let Some(why) = username_problem(&username) {
        return refuse(&state, viewer, why).await;
    }
    if let Some(next) = next_rename(db::username_changed_at(&state.db, viewer.account.id).await?.as_deref()) {
        let when = next.format("%-d %B %Y");
        return refuse(&state, viewer, crate::copy::rename_wait(&when.to_string())).await;
    }
    if !db::rename_account(&state.db, viewer.account.id, &username, true).await? {
        return refuse(&state, viewer, crate::copy::USERNAME_TAKEN).await;
    }
    Ok(Redirect::to("/account?msg=renamed").into_response())
}

#[derive(Deserialize)]
pub struct UnlinkForm {
    pub provider: String,
}

pub async fn unlink(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<UnlinkForm>,
) -> Result<Response, AppError> {
    let viewer = match signed_in(&state, &jar).await? {
        Ok(v) => v,
        Err(away) => return Ok(away),
    };
    match db::unlink_identity(&state.db, viewer.account.id, &form.provider).await? {
        Unlinked::Removed | Unlinked::NotLinked => Ok(Redirect::to("/account?msg=unlinked").into_response()),
        Unlinked::LastOne => refuse(&state, viewer, crate::copy::LAST_LOGIN).await,
        Unlinked::Plays42 => {
            refuse(&state, viewer, crate::copy::KEEP_42).await
        }
    }
}

#[derive(Deserialize)]
pub struct PasswordForm {
    /// Present when changing a password: the one in use now.
    #[serde(default)]
    pub current: Option<String>,
    /// Present when adding a password to an account that has none.
    #[serde(default)]
    pub email: Option<String>,
    pub password: String,
}

pub async fn password(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<PasswordForm>,
) -> Result<Response, AppError> {
    let viewer = match signed_in(&state, &jar).await? {
        Ok(v) => v,
        Err(away) => return Ok(away),
    };
    let id = viewer.account.id;
    let username = viewer.account.username.clone();

    match db::password_email(&state.db, id).await? {
        // Changing it: the current one first, under the same limit as signing in.
        Some((email, _)) => {
            let key = format!("login:email:{email}");
            if !state.limits.allows(&key, limits::LOGIN_PER_EMAIL) {
                return refuse(&state, viewer, crate::copy::TOO_MANY).await;
            }
            let stored = db::password_login(&state.db, &email).await?.map(|f| f.1);
            let current = form.current.unwrap_or_default();
            if !credentials::verify_password(&current, stored.as_deref()).await? {
                state.limits.record(&key);
                return refuse(&state, viewer, crate::copy::PASSWORD_WRONG).await;
            }
            if let Some(why) = password_problem(&form.password, &email, &username) {
                return refuse(&state, viewer, why).await;
            }
            let hash = credentials::hash_password(&form.password).await?;
            // Ends every session, this one included; this browser gets a new one.
            db::set_password(&state.db, id, &hash).await?;
            let token = auth::random_token();
            db::create_session(&state.db, &token, id, auth::SESSION_TTL_DAYS).await?;
            let jar = jar.add(auth::session_cookie(token, state.cfg.secure_cookies));
            Ok((jar, Redirect::to("/account?msg=password_changed")).into_response())
        }
        // Adding one.
        None => {
            if !state.cfg.email_signup() {
                return refuse(&state, viewer, crate::copy::NO_EMAIL_SIGNIN).await;
            }
            let email = match normalize_email(form.email.as_deref().unwrap_or_default()) {
                Ok(e) => e,
                Err(why) => return refuse(&state, viewer, why).await,
            };
            if let Some(why) = password_problem(&form.password, &email, &username) {
                return refuse(&state, viewer, why).await;
            }
            let hash = credentials::hash_password(&form.password).await?;
            if !db::add_password_login(&state.db, id, &email, &hash).await? {
                return refuse(&state, viewer, crate::copy::EMAIL_TAKEN).await;
            }
            if state.limits.take(&format!("mail:{email}"), limits::MAIL_PER_ADDRESS) {
                send_verification(&state, id, &email).await?;
            }
            Ok(Redirect::to("/account?msg=password_added").into_response())
        }
    }
}

#[derive(Deserialize)]
pub struct DeleteForm {
    pub confirm: String,
}

pub async fn delete(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<DeleteForm>,
) -> Result<Response, AppError> {
    let viewer = match signed_in(&state, &jar).await? {
        Ok(v) => v,
        Err(away) => return Ok(away),
    };
    if normalize_username(&form.confirm) != viewer.account.username {
        return refuse(&state, viewer, crate::copy::CONFIRM_USERNAME).await;
    }
    db::delete_account(&state.db, viewer.account.id).await?;
    tracing::info!(account = viewer.account.id, "account deleted by its owner");
    let jar = jar.remove(auth::clearing_cookie());
    Ok((jar, Redirect::to("/g?msg=deleted")).into_response())
}

#[cfg(test)]
mod tests {
    use axum::extract::{Query, State};
    use axum::http::StatusCode;
    use axum::Form;
    use axum_extra::extract::cookie::CookieJar;

    use super::*;
    use crate::testkit::{app_with, body, config, jar, location, set_cookie, stranger, student};

    async fn setup() -> (crate::db::test_support::TempDb, AppState, std::sync::Arc<std::sync::Mutex<Vec<crate::mail::Mail>>>) {
        app_with(config(&["megardes"])).await
    }

    async fn id_of(db: &db::Db, token: &str) -> i64 {
        db::viewer_for_session(db, token).await.unwrap().unwrap().account.id
    }

    async fn view(state: &AppState, token: &str) -> (StatusCode, String) {
        let resp = account(State(state.clone()), jar(token), Query(AccountQuery::default())).await.unwrap();
        let status = resp.status();
        (status, body(resp).await)
    }

    async fn with_password(state: &AppState, token: &str, email: &str, password: &str) {
        let resp = super::password(
            State(state.clone()),
            jar(token),
            Form(PasswordForm { current: None, email: Some(email.into()), password: password.into() }),
        )
        .await
        .unwrap();
        assert_eq!(location(&resp), "/account?msg=password_added");
    }

    async fn rename_to(state: &AppState, token: &str, name: &str) -> axum::response::Response {
        rename(State(state.clone()), jar(token), Form(RenameForm { username: name.into() })).await.unwrap()
    }

    #[tokio::test]
    async fn the_page_needs_a_signed_in_visitor() {
        let (_t, state, _) = setup().await;
        let resp = account(State(state.clone()), CookieJar::new(), Query(AccountQuery::default())).await.unwrap();
        assert_eq!(location(&resp), "/login");
        let resp = delete(State(state.clone()), CookieJar::new(), Form(DeleteForm { confirm: "x".into() })).await.unwrap();
        assert_eq!(location(&resp), "/login");
    }

    #[tokio::test]
    async fn a_42_student_sees_their_logins_and_cannot_drop_42() {
        let (t, state, _) = setup().await;
        student(&t.db, 1001, "megardes", "s").await;
        let (status, html) = view(&state, "s").await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains(">megardes<"));
        assert!(html.contains("your 42 Vienna round needs it"));

        let resp = unlink(State(state.clone()), jar("s"), Form(UnlinkForm { provider: "42".into() })).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(db::account_logins(&t.db, id_of(&t.db, "s").await).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn renaming_follows_the_rules_and_waits_a_month() {
        let (t, state, _) = setup().await;
        stranger(&t.db, "ann", "a").await;
        stranger(&t.db, "bob", "b").await;
        db::upsert_user(&t.db, 7, "megardes", "M", None).await.unwrap();

        for (name, says) in [("x", "3 to 20"), ("admin", "reserved"), ("bob", "taken"), ("MEGARDES", "taken")] {
            let resp = rename_to(&state, "a", name).await;
            assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "{name}");
            assert!(body(resp).await.contains(says), "{name}");
        }
        assert_eq!(location(&rename_to(&state, "a", " Annie ").await), "/account?msg=renamed");
        let v = db::viewer_for_session(&t.db, "a").await.unwrap().unwrap();
        assert_eq!(v.account.username, "annie");

        // Not again this month.
        let resp = rename_to(&state, "a", "annabel").await;
        assert!(body(resp).await.contains("next rename"));
        let (_, html) = view(&state, "a").await;
        assert!(html.contains("disabled"));

        // A month on, it can.
        sqlx::query("UPDATE accounts SET username_changed_at = '2000-01-01T00:00:00Z'").execute(&t.db).await.unwrap();
        assert_eq!(location(&rename_to(&state, "a", "annabel").await), "/account?msg=renamed");
    }

    #[tokio::test]
    async fn a_42_student_may_take_their_own_login_back() {
        let (t, state, _) = setup().await;
        student(&t.db, 1001, "megardes", "s").await;
        assert_eq!(location(&rename_to(&state, "s", "meg").await), "/account?msg=renamed");
        sqlx::query("UPDATE accounts SET username_changed_at = NULL").execute(&t.db).await.unwrap();
        // Their own 42 login is reserved against everyone but them.
        assert_eq!(location(&rename_to(&state, "s", "megardes").await), "/account?msg=renamed");
    }

    #[tokio::test]
    async fn adding_a_password_lets_a_provider_account_sign_in_by_email() {
        let (t, state, sent) = setup().await;
        student(&t.db, 1001, "megardes", "s").await;
        with_password(&state, "s", "Meg@Example.com", "correct horse battery").await;
        assert_eq!(sent.lock().unwrap().last().unwrap().purpose, "verify");

        let found = db::password_login(&t.db, "meg@example.com").await.unwrap();
        assert_eq!(found.map(|f| f.0), Some(id_of(&t.db, "s").await));
        let (_, html) = view(&state, "s").await;
        assert!(html.contains("signs in as meg@example.com"));
        assert!(html.contains("not confirmed yet"));

        // An email someone else signs in with cannot be added.
        stranger(&t.db, "ann", "a").await;
        let resp = super::password(
            State(state.clone()),
            jar("a"),
            Form(PasswordForm { current: None, email: Some("meg@example.com".into()), password: "another long password".into() }),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn changing_the_password_needs_the_current_one_and_signs_out_the_rest() {
        let (t, state, _) = setup().await;
        stranger(&t.db, "ann", "a").await;
        let id = id_of(&t.db, "a").await;
        with_password(&state, "a", "ann@example.com", "correct horse battery").await;
        db::create_session(&t.db, "elsewhere", id, 30).await.unwrap();

        let change = |current: &str, new: &str| {
            let state = state.clone();
            let form = PasswordForm { current: Some(current.into()), email: None, password: new.into() };
            async move { super::password(State(state), jar("a"), Form(form)).await.unwrap() }
        };
        let wrong = change("not it", "a whole new password").await;
        assert!(body(wrong).await.contains("not your current password"));
        let weak = change("correct horse battery", "short").await;
        assert_eq!(weak.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let resp = change("correct horse battery", "a whole new password").await;
        assert_eq!(location(&resp), "/account?msg=password_changed");
        let fresh = set_cookie(&resp, auth::SESSION_COOKIE).expect("this browser stays signed in");
        assert!(db::viewer_for_session(&t.db, &fresh).await.unwrap().is_some());
        assert!(db::viewer_for_session(&t.db, "elsewhere").await.unwrap().is_none());
        assert!(db::viewer_for_session(&t.db, "a").await.unwrap().is_none());
        let stored = db::password_login(&t.db, "ann@example.com").await.unwrap().unwrap().1;
        assert!(crate::credentials::verify_password("a whole new password", Some(&stored)).await.unwrap());
    }

    #[tokio::test]
    async fn the_last_way_in_cannot_be_removed() {
        let (t, state, _) = setup().await;
        stranger(&t.db, "ann", "a").await;
        let id = id_of(&t.db, "a").await;
        with_password(&state, "a", "ann@example.com", "correct horse battery").await;
        let resp = unlink(State(state.clone()), jar("a"), Form(UnlinkForm { provider: "email".into() })).await.unwrap();
        assert!(body(resp).await.contains("your only way in"));

        db::link_identity(&t.db, "github", "99", id, None).await.unwrap();
        let resp = unlink(State(state.clone()), jar("a"), Form(UnlinkForm { provider: "email".into() })).await.unwrap();
        assert_eq!(location(&resp), "/account?msg=unlinked");
        assert_eq!(db::password_email(&t.db, id).await.unwrap(), None, "the password goes with it");
        let logins: Vec<String> = db::account_logins(&t.db, id).await.unwrap().into_iter().map(|l| l.provider).collect();
        assert_eq!(logins, ["github"]);
    }

    #[tokio::test]
    async fn deleting_an_account_keeps_its_rounds_and_nothing_that_says_who() {
        let (t, state, _) = setup().await;
        student(&t.db, 1001, "megardes", "s").await;
        let id = id_of(&t.db, "s").await;
        with_password(&state, "s", "meg@example.com", "correct horse battery").await;
        db::insert_guess(&t.db, Pool::Global, "2026-09-05", id, 4, true).await.unwrap();
        db::insert_guess(&t.db, Pool::Vienna, "2026-09-05", 1001, 4, true).await.unwrap();

        let wrong = delete(State(state.clone()), jar("s"), Form(DeleteForm { confirm: "someone".into() })).await.unwrap();
        assert_eq!(wrong.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let resp = delete(State(state.clone()), jar("s"), Form(DeleteForm { confirm: "MegArdes".into() })).await.unwrap();
        assert_eq!(location(&resp), "/g?msg=deleted");
        assert!(db::viewer_for_session(&t.db, "s").await.unwrap().is_none());
        assert!(db::password_login(&t.db, "meg@example.com").await.unwrap().is_none());
        assert!(db::account_logins(&t.db, id).await.unwrap().is_empty());

        let r = db::round_summary(&t.db, Pool::Global, "2026-09-06", "2026-09-05").await.unwrap().unwrap();
        assert_eq!(r.winner_login, Some(format!("deleted-{id}")));
        // The 42 pool's record of them is the 42 pool's.
        let r = db::round_summary(&t.db, Pool::Vienna, "2026-09-06", "2026-09-05").await.unwrap().unwrap();
        assert_eq!(r.winner_login.as_deref(), Some("megardes"));

        // Signing in with 42 again starts afresh, under their login.
        let fresh = db::account_for_intra(&t.db, 1001, "megardes").await.unwrap();
        assert_ne!(fresh.id, id);
        assert_eq!(fresh.username, "megardes");
    }
}

// ------------------------------------------------------------ /welcome

#[derive(Deserialize, Default)]
pub struct WelcomeQuery {
    /// Where to go once the name is picked; only ever a page of the global
    /// round, so this cannot be turned into a redirect elsewhere.
    pub then: Option<String>,
}

fn after_welcome(then: Option<&str>) -> String {
    then.filter(|t| t.starts_with("/g?") || *t == "/g" || t.starts_with("/g/") && !t.starts_with("//"))
        .unwrap_or("/g?msg=welcome")
        .to_string()
}

/// The one step after a first sign-in through GitHub, Google or 42 from
/// another campus: the username, prefilled from the provider, to keep or
/// change. It is public on every result, so it is the player's choice.
pub async fn welcome(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(q): Query<WelcomeQuery>,
) -> Result<Response, AppError> {
    let Some(viewer) = current_viewer(&state, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    Ok(templates::render(&templates::WelcomeTemplate {
        pool: Pool::Global,
        test_mode: state.cfg.test_env(),
        impersonating: false,
        username: viewer.account.username.clone(),
        then: after_welcome(q.then.as_deref()),
        error: None,
        user: Some(UserView::from(viewer)),
    }))
}

#[derive(Deserialize)]
pub struct WelcomeForm {
    pub username: String,
    #[serde(default)]
    pub then: String,
}

pub async fn welcome_save(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<WelcomeForm>,
) -> Result<Response, AppError> {
    let Some(viewer) = current_viewer(&state, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    let then = after_welcome(Some(&form.then));
    let username = normalize_username(&form.username);
    if username != viewer.account.username {
        let problem = match username_problem(&username) {
            Some(why) => Some(why),
            None if !db::rename_account(&state.db, viewer.account.id, &username, false).await? => {
                Some(crate::copy::USERNAME_TAKEN)
            }
            None => None,
        };
        if let Some(why) = problem {
            let mut resp = templates::render(&templates::WelcomeTemplate {
                pool: Pool::Global,
                test_mode: state.cfg.test_env(),
                impersonating: false,
                username,
                then,
                error: Some(why),
                user: Some(UserView::from(viewer)),
            });
            *resp.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            return Ok(resp);
        }
    }
    Ok(Redirect::to(&then).into_response())
}
