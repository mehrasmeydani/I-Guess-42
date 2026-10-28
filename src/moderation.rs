//! `/mod`: keeping the global pool honest. Moderators are named by 42 login
//! in MODERATORS and work on the live site, unlike `/admin`, which only
//! exists on a test instance. They can ban and unban accounts, see groups
//! of accounts that may be one person playing several, and see how any
//! player came by their points.

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
use crate::templates::{self, ImprintTemplate, ModTemplate, Notice, PointsTemplate, PrivacyTemplate, UserView};

/// Accounts in a group before it is worth a moderator's look.
const CLUSTER_MIN: i64 = 3;

/// The signed-in moderator, or `None`, which callers answer with a 404 so
/// the page gives no sign that it exists.
async fn moderator(state: &AppState, jar: &CookieJar) -> Result<Option<Viewer>, AppError> {
    let viewer = current_viewer(state, jar).await?;
    Ok(viewer.filter(|v| is_moderator(state, v)))
}

/// Signed in with a 42 login named in MODERATORS.
pub fn is_moderator(state: &AppState, viewer: &Viewer) -> bool {
    viewer.intra.as_ref().is_some_and(|u| state.cfg.is_moderator(&u.login))
}

fn gone() -> Response {
    error_page(StatusCode::NOT_FOUND, crate::copy::NO_PAGE)
}

#[derive(Deserialize, Default)]
pub struct ModQuery {
    pub msg: Option<String>,
}

async fn page(state: &AppState, viewer: Viewer, notice: Option<Notice>, status: StatusCode) -> Result<Response, AppError> {
    let mut resp = templates::render(&ModTemplate {
        pool: Pool::Global,
        user: Some(UserView::from(viewer)),
        test_mode: state.cfg.test_env(),
        impersonating: false,
        notice,
        clusters: db::clusters(&state.db, CLUSTER_MIN).await?,
        warned: db::warned_usernames(&state.db).await?,
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
        Some(code @ ("banned" | "unbanned" | "adjusted" | "called_off" | "warned" | "mod_renamed")) => {
            crate::copy::notice(code).map(|(_, text)| Notice::ok(text))
        }
        Some(code @ "warned_no_mail") => crate::copy::notice(code).map(|(_, text)| Notice::error(text)),
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
        return refuse(crate::copy::NEED_REASON).await;
    }
    let Some(target) = db::mod_account(&state.db, &normalize_username(&form.username)).await? else {
        return refuse(crate::copy::NO_SUCH_ACCOUNT).await;
    };
    if target.id == viewer.account.id {
        return refuse(crate::copy::NOT_YOURSELF).await;
    }
    let open_round = Round::current(state.now()).key();
    db::ban_account(&state.db, target.id, reason, &open_round).await?;
    let by = viewer.intra.as_ref().map(|u| u.login.as_str()).unwrap_or("?");
    tracing::info!(moderator = by, account = target.id, username = %target.username, reason, "banned");
    Ok(Redirect::to("/mod?msg=banned").into_response())
}

#[derive(Deserialize)]
pub struct RenameForm {
    pub username: String,
    pub new_username: String,
}

/// Renames any account, the moderator's own included, without the monthly
/// wait and without starting one: for a name that offends, impersonates, or
/// that its owner cannot change back yet.
pub async fn rename(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<RenameForm>,
) -> Result<Response, AppError> {
    let Some(viewer) = moderator(&state, &jar).await? else {
        return Ok(gone());
    };
    let refuse = |why: &'static str| page(&state, viewer.clone(), Some(Notice::error(why)), StatusCode::UNPROCESSABLE_ENTITY);
    let Some(target) = db::mod_account(&state.db, &normalize_username(&form.username)).await? else {
        return refuse(crate::copy::NO_SUCH_ACCOUNT).await;
    };
    let new = normalize_username(&form.new_username);
    if let Some(why) = crate::credentials::username_problem_for_moderator(&new) {
        return refuse(why).await;
    }
    if !db::rename_account(&state.db, target.id, &new, false).await? {
        return refuse(crate::copy::USERNAME_TAKEN).await;
    }
    let by = viewer.intra.as_ref().map(|u| u.login.as_str()).unwrap_or("?");
    tracing::info!(moderator = by, account = target.id, from = %target.username, to = %new, "renamed");
    Ok(Redirect::to("/mod?msg=mod_renamed").into_response())
}

/// Warns an account by mail before a ban: the flag's reason, and what comes
/// next. Recorded on the account, so the flags show who has had theirs.
pub async fn warn(
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
        return refuse(crate::copy::NEED_REASON).await;
    }
    let Some(target) = db::mod_account(&state.db, &normalize_username(&form.username)).await? else {
        return refuse(crate::copy::NO_SUCH_ACCOUNT).await;
    };
    let by = viewer.intra.as_ref().map(|u| u.login.as_str()).unwrap_or("?");
    tracing::info!(moderator = by, account = target.id, username = %target.username, reason, "warned");
    let msg = match db::warn_account(&state.db, target.id, reason).await? {
        Some(email) => {
            crate::signin::send(&state, crate::mail::warning(&email, &target.username, reason, &state.cfg.public_url)).await;
            "warned"
        }
        None => "warned_no_mail",
    };
    Ok(Redirect::to(&format!("/mod?msg={msg}")).into_response())
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

#[derive(Deserialize)]
pub struct AdjustForm {
    pub username: String,
    pub amount: String,
    #[serde(default)]
    pub note: String,
}

/// Adds or takes points by hand, with a note the player sees in their
/// history: for undoing what a ban could not, such as a pot won with help.
pub async fn adjust(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<AdjustForm>,
) -> Result<Response, AppError> {
    let Some(viewer) = moderator(&state, &jar).await? else {
        return Ok(gone());
    };
    let refuse = |why: &'static str| page(&state, viewer.clone(), Some(Notice::error(why)), StatusCode::UNPROCESSABLE_ENTITY);
    let note = form.note.trim();
    if note.is_empty() {
        return refuse(crate::copy::NEED_NOTE).await;
    }
    let Ok(amount) = form.amount.trim().parse::<i64>() else {
        return refuse(crate::copy::AMOUNT_BAD).await;
    };
    if amount == 0 || amount.abs() > 1_000_000 {
        return refuse(crate::copy::AMOUNT_RANGE).await;
    }
    let Some(target) = db::mod_account(&state.db, &normalize_username(&form.username)).await? else {
        return refuse(crate::copy::NO_SUCH_ACCOUNT).await;
    };
    let open = Round::current(state.now()).key();
    let reference = format!("adjust:{}", crate::auth::random_token());
    db::credit_once(&state.db, target.id, &open, "adjust", amount, &reference, Some(note)).await?;
    let by = viewer.intra.as_ref().map(|u| u.login.as_str()).unwrap_or("?");
    tracing::info!(moderator = by, account = target.id, amount, note, "points adjusted");
    Ok(Redirect::to("/mod?msg=adjusted").into_response())
}

#[derive(Deserialize)]
pub struct PointsQuery {
    #[serde(default)]
    pub username: String,
}

/// `/mod/points`: a player's points history, line by line, as they see it
/// on their own points page: where a balance that looks too good came from.
/// Read only, so nothing is collected for them on the way.
pub async fn points(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(q): Query<PointsQuery>,
) -> Result<Response, AppError> {
    let Some(viewer) = moderator(&state, &jar).await? else {
        return Ok(gone());
    };
    let Some(target) = db::mod_account(&state.db, &normalize_username(&q.username)).await? else {
        return page(&state, viewer, Some(Notice::error(crate::copy::NO_SUCH_ACCOUNT)), StatusCode::NOT_FOUND).await;
    };
    Ok(templates::render(&PointsTemplate {
        pool: Pool::Global,
        test_mode: state.cfg.test_env(),
        impersonating: false,
        whose: Some(target.username),
        balance_label: templates::points_label(&db::balance(&state.db, target.id).await?),
        rows: crate::points_pages::history(&state.db, target.id, false).await?,
        stake: crate::economy::GUESS_STAKE,
        grant: crate::economy::DAILY_GRANT,
        bonus: crate::economy::SIGNUP_BONUS,
        user: Some(UserView::from(viewer)),
    }))
}

#[derive(Deserialize)]
pub struct VoidForm {
    pub round: String,
}

/// Calls off a global round that has not been settled: the open one, or
/// one that just closed. Every stake comes back and nobody wins it.
pub async fn void(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<VoidForm>,
) -> Result<Response, AppError> {
    let Some(viewer) = moderator(&state, &jar).await? else {
        return Ok(gone());
    };
    let date = form.round.trim();
    if chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_err() {
        let why = Notice::error(crate::copy::ROUND_DATE);
        return page(&state, viewer, Some(why), StatusCode::UNPROCESSABLE_ENTITY).await;
    }
    if !db::void_round(&state.db, date).await? {
        let why = Notice::error(crate::copy::ROUND_SETTLED);
        return page(&state, viewer, Some(why), StatusCode::UNPROCESSABLE_ENTITY).await;
    }
    let by = viewer.intra.as_ref().map(|u| u.login.as_str()).unwrap_or("?");
    tracing::info!(moderator = by, round = date, "global round called off");
    Ok(Redirect::to("/mod?msg=called_off").into_response())
}

// --------------------------------------------------------- legal pages

pub async fn privacy(State(state): State<AppState>, jar: CookieJar) -> Result<Response, AppError> {
    let viewer = current_viewer(&state, &jar).await?;
    Ok(templates::render(&PrivacyTemplate {
        pool: Pool::Global,
        user: viewer.map(UserView::from),
        test_mode: state.cfg.test_env(),
        impersonating: false,
    }))
}

pub async fn imprint(State(state): State<AppState>, jar: CookieJar) -> Result<Response, AppError> {
    let viewer = current_viewer(&state, &jar).await?;
    Ok(templates::render(&ImprintTemplate {
        pool: Pool::Global,
        user: viewer.map(UserView::from),
        test_mode: state.cfg.test_env(),
        impersonating: false,
        operator: crate::templates::operator_sentence(
            state.cfg.imprint_name.as_deref(),
            state.cfg.imprint_address.as_deref(),
            state.cfg.imprint_email.as_deref(),
        ),
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
    async fn moderators_rename_anyone_and_themselves_without_the_wait() {
        let (t, state) = setup(&["megardes"]).await;
        student(&t.db, 1001, "megardes", "mod").await;
        stranger(&t.db, "ann", "ann").await;
        // Ann renamed herself just now: her own wait has started.
        sqlx::query("UPDATE accounts SET username = 'rude', username_changed_at = '2099-01-01T00:00:00Z' WHERE username = 'ann'").execute(&t.db).await.unwrap();
        let ren = |who: &str, to: &str| {
            let form = Form(RenameForm { username: who.into(), new_username: to.into() });
            rename(State(state.clone()), jar("mod"), form)
        };
        assert_eq!(location(&ren("rude", "ann").await.unwrap()), "/mod?msg=mod_renamed");
        let (name, changed): (String, Option<String>) = sqlx::query_as("SELECT username, username_changed_at FROM accounts WHERE username = 'ann'").fetch_one(&t.db).await.unwrap();
        assert_eq!((name.as_str(), changed.as_deref()), ("ann", Some("2099-01-01T00:00:00Z")), "renamed, and no new wait started");
        // A hidden test account is a moderator's to name; nobody picks one.
        assert_eq!(location(&ren("ann", "test_00").await.unwrap()), "/mod?msg=mod_renamed");
        assert_eq!(crate::credentials::username_problem("test_00"), Some(crate::copy::USERNAME_RESERVED));
        assert_eq!(location(&ren("test_00", "ann").await.unwrap()), "/mod?msg=mod_renamed");
        // Taken, malformed or unknown: refused.
        assert_eq!(ren("ann", "megardes").await.unwrap().status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(ren("ann", "a").await.unwrap().status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(ren("nobody", "fine").await.unwrap().status(), StatusCode::UNPROCESSABLE_ENTITY);
        // Not a moderator: the page is not there.
        let form = Form(RenameForm { username: "ann".into(), new_username: "x-ann".into() });
        assert_eq!(rename(State(state.clone()), jar("ann"), form).await.unwrap().status(), StatusCode::NOT_FOUND);

        // Their own account, twice in a row from /account: no wait.
        for to in ["meg-one", "meg-two"] {
            let form = Form(crate::account::RenameForm { username: to.into() });
            let resp = crate::account::rename(State(state.clone()), jar("mod"), form).await.unwrap();
            assert_eq!(location(&resp), "/account?msg=renamed", "{to}");
        }
    }

    #[tokio::test]
    async fn moderators_see_how_any_player_got_their_points() {
        let (t, state) = setup(&["megardes"]).await;
        student(&t.db, 1001, "megardes", "mod").await;
        stranger(&t.db, "ann", "ann").await;
        let ann = db::mod_account(&t.db, "ann").await.unwrap().unwrap().id;
        db::credit_once(&t.db, ann, "2026-10-21", "adjust", 250, "adjust:t", Some("found a bug")).await.unwrap();
        db::credit_once(&t.db, ann, "2026-10-21", "stake", -10, "stake:t", None).await.unwrap();
        let limits = db::BetLimits { max_profit: 1_000_000, keep: 0 };
        db::place_bet(&t.db, ann, "2026-10-21", "range:2:3", 40, limits, |_| Some(350)).await.unwrap();
        let look = |token: &'static str, name: &str| {
            points(State(state.clone()), jar(token), Query(PointsQuery { username: name.into() }))
        };
        let resp = look("mod", "Ann").await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let html = body(resp).await;
        assert!(html.contains("ann&rsquo;s points") || html.contains("ann’s points"));
        assert!(html.contains("adjusted by a moderator: found a bug") && html.contains("+250"));
        assert!(html.contains(">guess<") && !html.contains("your guess"), "said of them, not you");
        assert!(html.contains(r#"href="/mod""#));
        assert!(html.contains("bet: winner 2 to 3 at ×3.50") && html.contains("-40"), "what the points went on");
        // The points on the leaderboard open it, for a moderator only.
        let board = |token: &'static str| {
            let (state, q) = (state.clone(), Query(crate::points_pages::BoardQuery::default()));
            async move { body(crate::points_pages::leaderboard(State(state), jar(token), q).await.unwrap()).await }
        };
        assert!(board("mod").await.contains(r#"href="/mod/points?username=ann""#));
        assert!(!board("ann").await.contains("/mod/points"));
        // Unknown: back on /mod, saying so.
        assert_eq!(look("mod", "nobody").await.unwrap().status(), StatusCode::NOT_FOUND);
        // Not a moderator: the page is not there, not even for their own.
        assert_eq!(look("ann", "ann").await.unwrap().status(), StatusCode::NOT_FOUND);
        // /mod links each listed name to it.
        db::ban_account(&t.db, ann, "test", "2026-10-22").await.unwrap();
        let html = body(moderation(State(state.clone()), jar("mod"), Query(ModQuery::default())).await.unwrap()).await;
        assert!(html.contains(r#"href="/mod/points?username=ann""#) && html.contains(r#"action="/mod/points""#));
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
        // Yesterday's round settled at 12:42, as it would have.
        crate::economy::settle_all(&t.db, &today).await.unwrap();
        let resp = ban(State(state.clone()), jar("mod"), ban_form("cheat", "")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "a reason is required");
        let resp = ban(State(state.clone()), jar("mod"), ban_form("nobody", "x")).await.unwrap();
        assert!(body(resp).await.contains("no account by that name"));
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
    async fn a_warning_is_mailed_recorded_and_shown_before_a_ban() {
        let mut cfg = config(&[]);
        cfg.moderators = vec!["megardes".into()];
        let (t, state, sent) = app_with(cfg).await;
        student(&t.db, 1001, "megardes", "mod").await;
        let hash = crate::credentials::hash_password("correct horse battery").await.unwrap();
        db::create_password_account(&t.db, "bot@example.com", "botty", &hash, None).await.unwrap();
        stranger(&t.db, "nomail", "n").await;
        let form = |u: &str, r: &str| Form(BanForm { username: u.into(), reason: r.into() });

        let resp = warn(State(state.clone()), jar("mod"), form("botty", "guess within ten seconds of each other")).await.unwrap();
        assert_eq!(location(&resp), "/mod?msg=warned");
        let mail = sent.lock().unwrap().last().cloned().expect("a mail");
        assert_eq!((mail.to.as_str(), mail.purpose), ("bot@example.com", "warning"));
        assert!(mail.text.contains("guess within ten seconds") && mail.text.contains("banned and marked"));
        assert_eq!(db::warned_usernames(&t.db).await.unwrap(), ["botty"]);

        // No address on file: warned in the record, and the moderator told.
        let resp = warn(State(state.clone()), jar("mod"), form("nomail", "x")).await.unwrap();
        assert_eq!(location(&resp), "/mod?msg=warned_no_mail");
        // Not a moderator: no page, nothing done.
        stranger(&t.db, "eve", "e").await;
        let resp = warn(State(state.clone()), jar("e"), form("eve", "x")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(db::warned_usernames(&t.db).await.unwrap().len(), 2);
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
    async fn a_winner_helped_by_their_own_accounts_and_accounts_in_lockstep_are_flagged() {
        let (t, _state) = setup(&["megardes"]).await;
        let id = |name: &'static str| {
            let db = t.db.clone();
            async move {
                sqlx::query("INSERT INTO accounts (username, created_at, last_seen_at) VALUES (?1, 'x', 'x')")
                    .bind(name).execute(&db).await.unwrap();
                let (id,): (i64,) = sqlx::query_as("SELECT id FROM accounts WHERE username = ?1").bind(name).fetch_one(&db).await.unwrap();
                id
            }
        };
        let day = |back: i64| (chrono::Utc::now() - chrono::Duration::days(back)).format("%Y-%m-%d").to_string();
        let guess = |round: String, who: i64, value: i64, net: &'static str| {
            let db = t.db.clone();
            async move {
                db::insert_guess(&db, Pool::Global, &round, who, value, true).await.unwrap();
                db::set_guess_ip_hash(&db, &round, who, net).await.unwrap();
            }
        };
        // Two accounts on the winner's network take 1 together, so the
        // winner's 2 is the lowest left unique. Someone elsewhere picks 3.
        let (w, x, y, z) = (id("winner").await, id("blockx").await, id("blocky").await, id("honest").await);
        let round = day(2);
        guess(round.clone(), x, 1, "nnnn").await;
        guess(round.clone(), y, 1, "nnnn").await;
        guess(round.clone(), w, 2, "nnnn").await;
        guess(round.clone(), z, 3, "other").await;
        db::settle_round(&t.db, &round).await.unwrap();

        // Two accounts guessing together in three rounds.
        let (l1, l2) = (id("lock1").await, id("lock2").await);
        for back in 3..=5 {
            guess(day(back), l1, 7, "l1").await;
            guess(day(back), l2, 8, "l2").await;
        }

        let found = db::clusters(&t.db, 3).await.unwrap();
        let serious: Vec<&db::Cluster> = found.iter().filter(|c| c.serious).collect();
        let blocked = serious.iter().find(|c| c.why.contains("winner won with 2")).expect("the helped winner");
        assert!(blocked.why.contains("blockx (1)") && blocked.why.contains("blocky (1)"), "{}", blocked.why);
        assert_eq!(blocked.usernames.len(), 3);
        assert!(!blocked.usernames.contains(&"honest".to_string()));
        assert!(serious.iter().any(|c| c.usernames == ["lock1", "lock2"]), "{found:?}");
        assert!(found.first().is_some_and(|c| c.serious), "serious ones first");
    }

    #[tokio::test]
    async fn number_killing_pairs_logins_together_and_clockwork_are_flagged() {
        let (t, _state) = setup(&["megardes"]).await;
        let mut ids = std::collections::HashMap::new();
        for name in ["k1", "k2", "win", "fan1", "fan2", "fan3", "bot1", "bot2"] {
            sqlx::query("INSERT INTO accounts (username, created_at, last_seen_at) VALUES (?1, 'x', 'x')")
                .bind(name).execute(&t.db).await.unwrap();
            let (id,): (i64,) = sqlx::query_as("SELECT id FROM accounts WHERE username = ?1").bind(name).fetch_one(&t.db).await.unwrap();
            ids.insert(name, id);
        }
        let now = chrono::Utc::now();
        let day = |back: i64| (now - chrono::Duration::days(back)).format("%Y-%m-%d").to_string();
        // Three rounds: k1 and k2 alone on 2, the three fans together on 1,
        // win takes 5. Different networks, different times: only the pair is
        // a pattern - the fans are a crowd, not a pair.
        for back in 2..=4 {
            let round = day(back);
            for (who, value) in [("k1", 2), ("k2", 2), ("fan1", 1), ("fan2", 1), ("fan3", 1), ("win", 5)] {
                db::insert_guess(&t.db, Pool::Global, &round, ids[who], value, true).await.unwrap();
                // Spread out over the day, so nothing looks like lockstep.
                let at = (now - chrono::Duration::days(back) + chrono::Duration::minutes(ids[who] * 47)).to_rfc3339();
                sqlx::query("UPDATE global_guesses SET submitted_at = ?1 WHERE round_date = ?2 AND user_id = ?3")
                    .bind(at).bind(&round).bind(ids[who]).execute(&t.db).await.unwrap();
            }
            db::settle_round(&t.db, &round).await.unwrap();
        }
        // bot1 and bot2 sign in within seconds of each other on three days,
        // and guess at 07:00 every day for five.
        for back in 2..=6 {
            let morning = (now - chrono::Duration::days(back)).date_naive().and_hms_opt(7, 0, 0).unwrap().and_utc();
            for (who, offset) in [("bot1", 0), ("bot2", 4)] {
                let at = (morning + chrono::Duration::seconds(offset)).to_rfc3339();
                if back <= 4 {
                    sqlx::query("INSERT INTO account_sessions (token, account_id, created_at, expires_at) VALUES (?1, ?2, ?3, '2099-01-01T00:00:00Z')")
                        .bind(format!("{who}{back}")).bind(ids[who]).bind(&at).execute(&t.db).await.unwrap();
                }
                let round = day(back + 10);
                db::insert_guess(&t.db, Pool::Global, &round, ids[who], 30 + offset, true).await.unwrap();
                sqlx::query("UPDATE global_guesses SET submitted_at = ?1 WHERE round_date = ?2 AND user_id = ?3")
                    .bind(&at).bind(&round).bind(ids[who]).execute(&t.db).await.unwrap();
            }
            db::settle_round(&t.db, &day(back + 10)).await.unwrap();
        }

        let found = db::clusters(&t.db, 3).await.unwrap();
        let with = |text: &str| found.iter().find(|c| c.why.contains(text)).unwrap_or_else(|| panic!("{text}: {found:?}"));
        let killers = with("kill numbers in pairs");
        assert_eq!(killers.usernames, ["k1", "k2"]);
        assert!(killers.serious && killers.why.contains("winners: win"));
        assert!(!found.iter().any(|c| c.usernames.contains(&"fan1".to_string())), "a crowd on 1 is not a pattern: {found:?}");
        assert_eq!(with("sign in within ten seconds").usernames, ["bot1", "bot2"]);
        let clock = with("at about 07:00 UTC every day");
        assert_eq!(clock.usernames, ["bot1", "bot2"]);
        assert!(clock.serious, "two accounts on one clock");
    }

    async fn page_of(cfg: crate::config::Config, imprint_page: bool) -> String {
        let (_t, state, _) = app_with(cfg).await;
        let jar = axum_extra::extract::cookie::CookieJar::new();
        let resp = if imprint_page {
            imprint(State(state), jar).await.unwrap()
        } else {
            privacy(State(state), jar).await.unwrap()
        };
        body(resp).await
    }

    #[tokio::test]
    async fn the_imprint_names_the_operator_in_production_only() {
        let mut cfg = config(&[]);
        cfg.environment = Some("production".into());
        cfg.imprint_name = Some("Jane Doe".into());
        cfg.imprint_email = Some("hi@example.com".into());
        let html = page_of(cfg.clone(), true).await;
        assert!(html.contains("i guess low is run by Jane Doe. contact: hi@example.com"));
        assert!(html.contains(">impressum</h1>"));
        assert_eq!(html.matches("<h1").count(), 1, "exactly one h1");
        assert!(!html.contains("not given"));

        // A test instance says so instead of showing anyone's details.
        cfg.environment = Some("test".into());
        let html = page_of(cfg, true).await;
        assert!(html.contains("operator details not shown on test instance"));
        assert!(!html.contains("Jane Doe"));
        assert_eq!(html.matches("<h1").count(), 1);
    }

    #[test]
    fn the_operator_sentence_leaves_out_what_is_missing() {
        use crate::templates::operator_sentence;
        assert_eq!(
            operator_sentence(Some("Jane"), Some("Wien"), Some("a@b.at")),
            "i guess low is run by Jane, Wien. contact: a@b.at"
        );
        assert_eq!(operator_sentence(None, None, Some("a@b.at")), "i guess low is run by students, for students. contact: a@b.at");
        for s in [operator_sentence(None, None, None), operator_sentence(Some("J"), None, None)] {
            assert!(!s.contains("not given") && !s.contains("None"), "{s}");
        }
    }

    #[tokio::test]
    async fn the_privacy_page_renders() {
        assert!(page_of(config(&[]), false).await.contains("cookie"));
    }
}
