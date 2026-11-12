//! Custom sessions: a round of its own that a moderator opens for as long
//! as they choose, for a room full of people - a talk, an event, a class.
//! The moderator gets a QR code to put on the screen. Whoever scans it
//! signs in (or makes an account) and plays under their username, or plays
//! as a guest under a name of their own for this session; then they pick a
//! number. A cookie remembers who they are until well after it ends, so
//! they come back to their own number and then to the result, and the
//! round page shows them a button to each open session they are in - and
//! nobody else any. Same rule as the daily rounds: the lowest number only
//! one person picked wins.
//!
//! A guest can play the same session twice by clearing their cookies: this
//! is a game for a room, where the room keeps people honest, not a round
//! anything is paid out on.

use std::fmt::Write;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chrono::{DateTime, TimeDelta, Utc};
use rand::seq::SliceRandom;
use serde::Deserialize;

use crate::app::{error_page, AppError, AppState};
use crate::db::{self, CustomPlayer, CustomSession, Pool, Viewer};
use crate::handlers::{current_viewer, parse_guess};
use crate::moderation::is_moderator;
use crate::templates::{
    self, group_digits, CustomLink, CustomListRow, CustomModTemplate, CustomPick, CustomPlayTemplate, CustomQrTemplate,
    Notice, UserView,
};

/// The cookie that says who you are in custom sessions: one random token
/// per browser, kept in every session it joins, and sent to every page so
/// the round page can find your sessions.
pub const PLAYER_COOKIE: &str = "ig42_custom";
/// How long the cookie lasts from the last join: the longest session and
/// a month after it, to come back to the result.
const COOKIE_DAYS: i64 = 7 + 30;
/// The code of the session to come back to after signing in or making an
/// account from its page; read by `signin::signed_in_to`.
pub const RETURN_COOKIE: &str = "ig42_custom_back";
/// Long enough to make an account and confirm the email.
const RETURN_MINUTES: i64 = 60;
/// The longest session a moderator can open: a week.
pub const MAX_MINUTES: i64 = 7 * 24 * 60;
const NAME_MAX: usize = 60;
const PLAYER_NAME_MAX: usize = 20;
/// Sessions listed on the moderator's page.
const LISTED: i64 = 30;
/// Letters and digits that cannot be mistaken for each other when typed
/// from a slide: no 0/o, 1/l/i.
const CODE_CHARS: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
const CODE_LEN: usize = 6;

fn gone() -> Response {
    error_page(StatusCode::NOT_FOUND, crate::copy::NO_PAGE)
}

fn new_code() -> String {
    let mut rng = rand::thread_rng();
    (0..CODE_LEN).map(|_| *CODE_CHARS.choose(&mut rng).expect("not empty") as char).collect()
}

/// A code as typed into the address bar, if it could be one.
fn clean_code(raw: &str) -> Option<String> {
    let code = raw.trim().to_ascii_lowercase();
    (code.len() == CODE_LEN && code.bytes().all(|b| CODE_CHARS.contains(&b))).then_some(code)
}

/// Whether `path` is a session's page, "/s/abc234", and nothing more.
pub fn is_session_page(path: &str) -> bool {
    path.strip_prefix("/s/").and_then(clean_code).is_some_and(|code| path == format!("/s/{code}"))
}

/// Where a session is played, as the QR code says it.
pub fn link(state: &AppState, code: &str) -> String {
    format!("{}/s/{code}", state.cfg.public_url)
}

/// A QR code of `text` as SVG: black squares on white with the usual quiet
/// zone, so it scans on a dark page, a projector and paper alike.
pub fn qr_svg(text: &str) -> String {
    let Ok(code) = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium) else {
        return String::new();
    };
    let quiet = 4;
    let n = code.size();
    let mut path = String::new();
    for y in 0..n {
        for x in 0..n {
            if code.get_module(x, y) {
                let _ = write!(path, "M{},{}h1v1h-1z", x + quiet, y + quiet);
            }
        }
    }
    let side = n + 2 * quiet;
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {side} {side}" shape-rendering="crispEdges"><rect width="{side}" height="{side}" fill="#fff"/><path d="{path}" fill="#000"/></svg>"##
    )
}

/// "closes 14:30" today, or with the date on any other day, on the Vienna
/// clock like the rest of the site.
fn when_label(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let local = at.with_timezone(&crate::round::TZ);
    if local.date_naive() == now.with_timezone(&crate::round::TZ).date_naive() {
        local.format("%H:%M").to_string()
    } else {
        local.format("%d.%m.%Y %H:%M").to_string()
    }
}

fn rfc(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// How much of a session has gone by, 0-100: the round page's bar, over
/// the session's own length instead of a day.
fn progress(session: &CustomSession, now: DateTime<Utc>) -> i64 {
    let span = (session.ends() - session.starts()).num_seconds().max(1);
    (now - session.starts()).num_seconds().clamp(0, span) * 100 / span
}

/// How long the result pops up over the session's page after it ends: as
/// long as the round page announces a closed round.
const ANNOUNCE_FOR: i64 = 30 * 60;

/// Every pick of a closed session with its fate, lowest first, and the
/// winner's place in that list.
fn picks(rows: Vec<db::CustomPickRow>, mine: Option<&str>) -> (Vec<CustomPick>, Option<usize>) {
    let count = |v: i64| rows.iter().filter(|r| r.guess == v).count();
    // Sorted by number, so the first number picked once is the winner.
    let winner = rows.iter().position(|r| count(r.guess) == 1);
    let out = rows
        .iter()
        .enumerate()
        .map(|(i, r)| CustomPick {
            name: r.name.clone(),
            value_label: group_digits(r.guess),
            fate: if Some(i) == winner {
                "won"
            } else if count(r.guess) == 1 {
                "once"
            } else {
                "taken"
            },
            guest: r.guest,
            mine: mine.is_some_and(|m| m.eq_ignore_ascii_case(&r.name)),
        })
        .collect();
    (out, winner)
}

/// The round page's link to every open session this browser or the
/// signed-in account joined; none for anyone else, so a session stays with
/// the room that has its code.
pub async fn open_links(state: &AppState, jar: &CookieJar, viewer: Option<&Viewer>) -> Result<Vec<CustomLink>, AppError> {
    let token = token(jar);
    let account = viewer.map(|v| v.account.id);
    if token.is_none() && account.is_none() {
        return Ok(Vec::new());
    }
    let now = Utc::now();
    Ok(db::open_custom_sessions_of(&state.db, token.as_deref(), account, now)
        .await?
        .into_iter()
        .map(|s| CustomLink {
            href: format!("/s/{}", s.code),
            closes: when_label(s.ends(), now),
            name: s.name,
        })
        .collect())
}

// ---------------------------------------------------------------- moderators

async fn moderator(state: &AppState, jar: &CookieJar) -> Result<Option<Viewer>, AppError> {
    let viewer = current_viewer(state, jar).await?;
    Ok(viewer.filter(|v| is_moderator(state, v)))
}

async fn list_page(state: &AppState, viewer: Viewer, notice: Option<Notice>, status: StatusCode) -> Result<Response, AppError> {
    let now = Utc::now();
    let sessions = db::recent_custom_sessions(&state.db, LISTED)
        .await?
        .into_iter()
        .map(|(s, players)| CustomListRow {
            open: s.is_open(now),
            when: when_label(s.ends(), now),
            href: format!("/mod/sessions/{}", s.code),
            name: s.name,
            players,
        })
        .collect();
    let mut resp = templates::render(&CustomModTemplate {
        pool: Pool::Global,
        user: Some(UserView::from(viewer)),
        test_mode: state.cfg.test_env(),
        impersonating: false,
        notice,
        sessions,
        max_minutes: MAX_MINUTES,
    });
    *resp.status_mut() = status;
    Ok(resp)
}

/// `GET /mod/sessions`: open a session, and the ones opened lately.
pub async fn list(State(state): State<AppState>, jar: CookieJar) -> Result<Response, AppError> {
    let Some(viewer) = moderator(&state, &jar).await? else {
        return Ok(gone());
    };
    list_page(&state, viewer, None, StatusCode::OK).await
}

#[derive(Deserialize)]
pub struct CreateForm {
    pub name: String,
    pub minutes: String,
}

/// `POST /mod/sessions`: opens a session from now for the minutes given,
/// and goes to its QR code.
pub async fn create(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<CreateForm>,
) -> Result<Response, AppError> {
    let Some(viewer) = moderator(&state, &jar).await? else {
        return Ok(gone());
    };
    let refuse = |why: &'static str| list_page(&state, viewer.clone(), Some(Notice::error(why)), StatusCode::UNPROCESSABLE_ENTITY);
    let name = form.name.trim();
    if name.is_empty() || name.chars().count() > NAME_MAX {
        return refuse(crate::copy::SESSION_NAME_BAD).await;
    }
    let minutes = match form.minutes.trim().parse::<i64>() {
        Ok(m) if (1..=MAX_MINUTES).contains(&m) => m,
        _ => return refuse(crate::copy::SESSION_LENGTH_BAD).await,
    };
    let ends_at = Utc::now() + TimeDelta::minutes(minutes);
    // 31^6 codes: a clash is rare, and a second draw all but never clashes.
    for _ in 0..5 {
        let code = new_code();
        if db::create_custom_session(&state.db, &code, name, viewer.account.id, ends_at).await? {
            let by = viewer.intra.as_ref().map(|u| u.login.as_str()).unwrap_or("?");
            tracing::info!(moderator = by, code, name, minutes, "custom session opened");
            return Ok(Redirect::to(&format!("/mod/sessions/{code}")).into_response());
        }
    }
    Err(anyhow::anyhow!("no free session code after five draws").into())
}

/// `GET /mod/sessions/{code}`: the QR code to show the room, how many
/// are in, and the result once it is over.
pub async fn show(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(code): Path<String>,
) -> Result<Response, AppError> {
    let Some(viewer) = moderator(&state, &jar).await? else {
        return Ok(gone());
    };
    let Some(session) = find(&state, &code).await? else {
        return Ok(gone());
    };
    let now = Utc::now();
    let (joined, guessed) = db::custom_counts(&state.db, session.id).await?;
    let open = session.is_open(now);
    let (rows, winner) = if open {
        (Vec::new(), None)
    } else {
        picks(db::custom_picks(&state.db, session.id).await?, None)
    };
    let link = link(&state, &session.code);
    Ok(templates::render(&CustomQrTemplate {
        pool: Pool::Global,
        user: Some(UserView::from(viewer)),
        test_mode: state.cfg.test_env(),
        impersonating: false,
        qr: qr_svg(&link),
        link,
        open,
        closes: when_label(session.ends(), now),
        time_left: crate::round::clock((session.ends() - now).num_seconds()),
        time_left_spoken: crate::round::spoken_duration((session.ends() - now).num_seconds()),
        starts_at: rfc(session.starts()),
        ends_at: rfc(session.ends()),
        server_now: rfc(now),
        progress_pct: progress(&session, now),
        joined,
        guessed,
        winner: winner.map(|i| rows[i].clone()),
        rows,
        code: session.code,
        name: session.name,
    }))
}

/// `GET /mod/sessions/{code}/qr.svg`: the QR code alone, to save or print.
pub async fn qr(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(code): Path<String>,
) -> Result<Response, AppError> {
    if moderator(&state, &jar).await?.is_none() {
        return Ok(gone());
    }
    let Some(session) = find(&state, &code).await? else {
        return Ok(gone());
    };
    let disposition = format!("attachment; filename=\"session-{}.svg\"", session.code);
    Ok((
        [(header::CONTENT_TYPE, "image/svg+xml".to_string()), (header::CONTENT_DISPOSITION, disposition)],
        qr_svg(&link(&state, &session.code)),
    )
        .into_response())
}

/// `POST /mod/sessions/{code}/end`: closes the session now and shows its
/// result.
pub async fn end(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(code): Path<String>,
) -> Result<Response, AppError> {
    let Some(viewer) = moderator(&state, &jar).await? else {
        return Ok(gone());
    };
    let Some(session) = find(&state, &code).await? else {
        return Ok(gone());
    };
    db::end_custom_session(&state.db, session.id).await?;
    let by = viewer.intra.as_ref().map(|u| u.login.as_str()).unwrap_or("?");
    tracing::info!(moderator = by, code = %session.code, "custom session ended early");
    Ok(Redirect::to(&format!("/mod/sessions/{}", session.code)).into_response())
}

// ---------------------------------------------------------------- players

async fn find(state: &AppState, raw: &str) -> Result<Option<CustomSession>, AppError> {
    match clean_code(raw) {
        Some(code) => Ok(db::custom_session(&state.db, &code).await?),
        None => Ok(None),
    }
}

/// This browser's token, if it has one of the shape `random_token` makes:
/// anything else was not handed out here, and is not taken on.
fn token(jar: &CookieJar) -> Option<String> {
    jar.get(PLAYER_COOKIE)
        .map(|c| c.value().to_string())
        .filter(|t| t.len() == 32 && t.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Who this browser is in `session`: by its cookie, or the account
/// signed in, if it joined.
async fn player(state: &AppState, jar: &CookieJar, session: &CustomSession) -> Result<Option<CustomPlayer>, AppError> {
    let token = token(jar);
    let account = current_viewer(state, jar).await?.map(|v| v.account.id);
    if token.is_none() && account.is_none() {
        return Ok(None);
    }
    Ok(db::custom_player(&state.db, session.id, token.as_deref(), account).await?)
}

fn return_cookie(state: &AppState, session: &CustomSession) -> Cookie<'static> {
    let mut cookie = Cookie::new(RETURN_COOKIE, session.code.clone());
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_secure(state.cfg.secure_cookies);
    cookie.set_path("/");
    cookie.set_max_age(time::Duration::minutes(RETURN_MINUTES));
    cookie
}

/// The session page to go back to after signing in, if one was left for
/// it, and the jar with that note taken out: it is used once.
pub fn take_return(jar: CookieJar) -> (CookieJar, Option<String>) {
    let Some(value) = jar.get(RETURN_COOKIE).map(|c| c.value().to_string()) else {
        return (jar, None);
    };
    let mut clear = Cookie::new(RETURN_COOKIE, "");
    clear.set_path("/");
    let back = clean_code(&value).map(|code| format!("/s/{code}"));
    (jar.remove(clear), back)
}

fn player_cookie(state: &AppState, token: String) -> Cookie<'static> {
    let mut cookie = Cookie::new(PLAYER_COOKIE, token);
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_secure(state.cfg.secure_cookies);
    cookie.set_path("/");
    cookie.set_max_age(time::Duration::days(COOKIE_DAYS));
    cookie
}

async fn play_page(
    state: &AppState,
    jar: &CookieJar,
    session: CustomSession,
    me: Option<CustomPlayer>,
    typed_name: &str,
    notice: Option<Notice>,
    status: StatusCode,
) -> Result<Response, AppError> {
    play_page_as(state, jar, session, me, typed_name, notice, status, false).await
}

#[allow(clippy::too_many_arguments)]
async fn play_page_as(
    state: &AppState,
    jar: &CookieJar,
    session: CustomSession,
    me: Option<CustomPlayer>,
    typed_name: &str,
    notice: Option<Notice>,
    status: StatusCode,
    just_locked: bool,
) -> Result<Response, AppError> {
    let now = Utc::now();
    let open = session.is_open(now);
    let (joined, guessed) = db::custom_counts(&state.db, session.id).await?;
    let (rows, winner) = if open {
        (Vec::new(), None)
    } else {
        picks(db::custom_picks(&state.db, session.id).await?, me.as_ref().map(|p| p.name.as_str()))
    };
    let viewer = current_viewer(state, jar).await?;
    let mut resp = templates::render(&CustomPlayTemplate {
        pool: Pool::Global,
        username: viewer.as_ref().map(|v| v.account.username.clone()),
        user: viewer.map(UserView::from),
        test_mode: state.cfg.test_env(),
        impersonating: crate::auth::admin_return_token(jar).is_some(),
        notice,
        href: format!("/s/{}", session.code),
        open,
        closes: when_label(session.ends(), now),
        time_left: crate::round::clock((session.ends() - now).num_seconds()),
        time_left_spoken: crate::round::spoken_duration((session.ends() - now).num_seconds()),
        starts_at: rfc(session.starts()),
        ends_at: rfc(session.ends()),
        server_now: rfc(now),
        progress_pct: progress(&session, now),
        joined,
        guessed,
        just_locked: just_locked && me.as_ref().is_some_and(|p| p.guess.is_some()),
        announce_left: Some(ANNOUNCE_FOR - (now - session.ends()).num_seconds())
            .filter(|left| !open && *left > 0 && *left <= ANNOUNCE_FOR),
        my_name: me.as_ref().map(|p| p.name.clone()),
        typed_name: typed_name.to_string(),
        // Back to the guest form, open, when it was what was refused.
        guest_open: !typed_name.is_empty(),
        my_guess: me.and_then(|p| p.guess).map(group_digits),
        winner: winner.map(|i| rows[i].clone()),
        rows,
        name: session.name,
    });
    *resp.status_mut() = status;
    Ok(resp)
}

/// `GET /s/{code}/signin` and `/s/{code}/signup`: off to sign in or make
/// an account, with a note to come back here afterwards.
pub async fn signin(State(state): State<AppState>, jar: CookieJar, Path(code): Path<String>) -> Result<Response, AppError> {
    away(state, jar, code, "/login").await
}

pub async fn signup(State(state): State<AppState>, jar: CookieJar, Path(code): Path<String>) -> Result<Response, AppError> {
    away(state, jar, code, "/signup").await
}

async fn away(state: AppState, jar: CookieJar, code: String, to: &str) -> Result<Response, AppError> {
    let Some(session) = find(&state, &code).await? else {
        return Ok(gone());
    };
    let jar = jar.add(return_cookie(&state, &session));
    Ok((jar, Redirect::to(to)).into_response())
}

#[derive(Deserialize, Default)]
pub struct PageQuery {
    pub msg: Option<String>,
}

/// `GET /s/{code}`: who you play as, then a number, then - once it is over
/// - who won.
pub async fn page(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(code): Path<String>,
    Query(q): Query<PageQuery>,
) -> Result<Response, AppError> {
    let Some(session) = find(&state, &code).await? else {
        return Ok(gone());
    };
    // The code in the QR code is lowercase; one typed in capitals still
    // has to land where the cookie's path is.
    if session.code != code {
        return Ok(Redirect::to(&format!("/s/{}", session.code)).into_response());
    }
    let me = player(&state, &jar, &session).await?;
    // "saved" is said by the sealed number itself, as on the round page.
    let just_locked = q.msg.as_deref() == Some("saved");
    play_page_as(&state, &jar, session, me, "", None, StatusCode::OK, just_locked).await
}

#[derive(Deserialize)]
pub struct JoinForm {
    /// The guest's name; empty to play as the account signed in.
    #[serde(default)]
    pub name: String,
    /// "account" to play under your username, anything else as a guest.
    #[serde(default)]
    pub mode: String,
}

/// `POST /s/{code}/join`: joins as the signed-in account, under its
/// username, or as a guest under a name for this session; and hands out
/// the cookie that remembers which.
pub async fn join(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(code): Path<String>,
    Form(form): Form<JoinForm>,
) -> Result<Response, AppError> {
    let Some(session) = find(&state, &code).await? else {
        return Ok(gone());
    };
    let back = format!("/s/{}", session.code);
    if player(&state, &jar, &session).await?.is_some() {
        return Ok(Redirect::to(&back).into_response());
    }
    if !session.is_open(Utc::now()) {
        let notice = Some(Notice::error(crate::copy::SESSION_OVER));
        return play_page(&state, &jar, session, None, &form.name, notice, StatusCode::UNPROCESSABLE_ENTITY).await;
    }
    // The browser's token from an earlier session, or a new one.
    let token = token(&jar).unwrap_or_else(crate::auth::random_token);
    if form.mode == "account" {
        // Signed out since the page was drawn: back to the choice.
        let Some(viewer) = current_viewer(&state, &jar).await? else {
            return Ok(Redirect::to(&back).into_response());
        };
        let name = viewer.account.username.clone();
        if !db::join_custom_session(&state.db, session.id, &token, Some(viewer.account.id), &name).await? {
            // A guest got the name first.
            let notice = Some(Notice::error(crate::copy::USERNAME_TAKEN_BY_GUEST));
            return play_page(&state, &jar, session, None, "", notice, StatusCode::UNPROCESSABLE_ENTITY).await;
        }
        let jar = jar.add(player_cookie(&state, token));
        return Ok((jar, Redirect::to(&back)).into_response());
    }
    let refuse = |why: &'static str| {
        play_page(&state, &jar, session.clone(), None, &form.name, Some(Notice::error(why)), StatusCode::UNPROCESSABLE_ENTITY)
    };
    let name: String = form.name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() || name.chars().count() > PLAYER_NAME_MAX || name.chars().any(char::is_control) {
        return refuse(crate::copy::PLAYER_NAME_BAD).await;
    }
    // Someone's username is theirs: a guest cannot pass as them.
    if db::is_username(&state.db, &name).await? {
        return refuse(crate::copy::GUEST_NAME_IS_USERNAME).await;
    }
    if !db::join_custom_session(&state.db, session.id, &token, None, &name).await? {
        return refuse(crate::copy::PLAYER_NAME_TAKEN).await;
    }
    let jar = jar.add(player_cookie(&state, token));
    Ok((jar, Redirect::to(&back)).into_response())
}

#[derive(Deserialize)]
pub struct GuessForm {
    pub guess: String,
}

/// `POST /s/{code}/guess`: locks the player's number in. Final, as in the
/// daily rounds.
pub async fn guess(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(code): Path<String>,
    Form(form): Form<GuessForm>,
) -> Result<Response, AppError> {
    let Some(session) = find(&state, &code).await? else {
        return Ok(gone());
    };
    let back = format!("/s/{}", session.code);
    // Not in it (the cookie expired, or another browser signed out): back
    // to the choice of who to play as.
    let Some(me) = player(&state, &jar, &session).await? else {
        return Ok(Redirect::to(&back).into_response());
    };
    let refuse = |why: &'static str, me: CustomPlayer| {
        play_page(&state, &jar, session.clone(), Some(me), "", Some(Notice::error(why)), StatusCode::UNPROCESSABLE_ENTITY)
    };
    if me.guess.is_some() {
        return Ok(Redirect::to(&back).into_response());
    }
    if !session.is_open(Utc::now()) {
        return refuse(crate::copy::SESSION_OVER, me).await;
    }
    let value = match parse_guess(&form.guess) {
        Ok(v) => v,
        Err(code) => {
            let why = crate::copy::notice(code).map(|(_, text)| text).unwrap_or(crate::copy::BROKE_HERE);
            return refuse(why, me).await;
        }
    };
    // Refused only if it closed, or another tab got there first, since the
    // checks above: the page says which.
    if db::custom_guess(&state.db, me.id, value).await? {
        return Ok(Redirect::to(&format!("{back}?msg=saved")).into_response());
    }
    Ok(Redirect::to(&back).into_response())
}

#[cfg(test)]
mod tests {
    use axum::extract::{Path, State};
    use axum::http::StatusCode;
    use axum::Form;
    use axum_extra::extract::cookie::CookieJar;

    use super::*;
    use crate::testkit::{app_with, body, config, jar, location, sent, set_cookie, student};

    async fn setup() -> (crate::db::test_support::TempDb, AppState) {
        let mut cfg = config(&[]);
        cfg.moderators = vec!["megardes".to_string()];
        let (t, state, _) = app_with(cfg).await;
        student(&t.db, 1001, "megardes", "mod").await;
        student(&t.db, 1002, "other", "student").await;
        (t, state)
    }

    async fn open(state: &AppState, minutes: &str) -> String {
        let form = CreateForm { name: "friday talk".into(), minutes: minutes.into() };
        let resp = create(State(state.clone()), jar("mod"), Form(form)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        location(&resp).trim_start_matches("/mod/sessions/").to_string()
    }

    /// Joins as `name` and returns the cookie's token.
    async fn join_as(state: &AppState, code: &str, name: &str) -> String {
        let resp = join(State(state.clone()), CookieJar::new(), Path(code.into()), Form(JoinForm { name: name.into(), mode: "guest".into() }))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SEE_OTHER, "{name}");
        set_cookie(&resp, PLAYER_COOKIE).unwrap()
    }

    async fn guess_as(state: &AppState, code: &str, token: &str, n: &str) -> StatusCode {
        guess(State(state.clone()), sent(&[(PLAYER_COOKIE, token)]), Path(code.into()), Form(GuessForm { guess: n.into() }))
            .await
            .unwrap()
            .status()
    }

    /// The round page's session buttons, for whoever sends `jar`.
    async fn links(state: &AppState, jar: CookieJar) -> Result<Vec<CustomLink>, AppError> {
        let viewer = current_viewer(state, &jar).await?;
        open_links(state, &jar, viewer.as_ref()).await
    }

    async fn page_for(state: &AppState, code: &str, token: Option<&str>) -> String {
        let jar = token.map(|t| sent(&[(PLAYER_COOKIE, t)])).unwrap_or_default();
        body(page(State(state.clone()), jar, Path(code.into()), Query(PageQuery::default())).await.unwrap()).await
    }

    #[tokio::test]
    async fn only_moderators_open_sessions() {
        let (_t, state) = setup().await;
        let form = || Form(CreateForm { name: "x".into(), minutes: "10".into() });
        let resp = create(State(state.clone()), jar("student"), form()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let resp = list(State(state.clone()), jar("student")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM custom_sessions").fetch_one(&state.db).await.unwrap();
        assert_eq!(n, 0);

        for bad in ["0", "-5", "x", &(MAX_MINUTES + 1).to_string()] {
            let f = Form(CreateForm { name: "x".into(), minutes: bad.into() });
            let resp = create(State(state.clone()), jar("mod"), f).await.unwrap();
            assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
        }
    }

    #[tokio::test]
    async fn the_moderator_gets_a_qr_code_of_the_link() {
        let (_t, state) = setup().await;
        let code = open(&state, "45").await;
        assert_eq!(code.len(), CODE_LEN);
        let html = body(show(State(state.clone()), jar("mod"), Path(code.clone())).await.unwrap()).await;
        assert!(html.contains("<svg"));
        assert!(html.contains(&format!("http://test/s/{code}")));
        let resp = show(State(state.clone()), jar("student"), Path(code)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_qr_code_reads_back_as_the_link() {
        let svg = qr_svg("http://test/s/abc234");
        // Draw the SVG's squares into a grid and decode it.
        let side: usize = svg.split("viewBox=\"0 0 ").nth(1).unwrap().split(' ').next().unwrap().parse().unwrap();
        let scale = 8;
        let mut dark = vec![false; side * side];
        let d = svg.split(" d=\"").nth(1).unwrap().split('"').next().unwrap();
        for m in d.split('M').filter(|s| !s.is_empty()) {
            let xy = m.split('h').next().unwrap();
            let (x, y) = xy.split_once(',').unwrap();
            dark[y.parse::<usize>().unwrap() * side + x.parse::<usize>().unwrap()] = true;
        }
        let mut img = rqrr::PreparedImage::prepare_from_greyscale(side * scale, side * scale, |x, y| {
            if dark[(y / scale) * side + x / scale] { 0 } else { 255 }
        });
        let grids = img.detect_grids();
        assert_eq!(grids.len(), 1);
        assert_eq!(grids[0].decode().unwrap().1, "http://test/s/abc234");
    }

    #[tokio::test]
    async fn players_join_by_cookie_guess_once_and_lowest_unique_wins() {
        let (_t, state) = setup().await;
        let code = open(&state, "30").await;

        let ana = join_as(&state, &code, "ana").await;
        // The round page links to it while it is open, for those in it.
        let shown = links(&state, sent(&[(PLAYER_COOKIE, &ana)])).await.unwrap();
        assert_eq!(shown[0].href, format!("/s/{code}"));
        let ben = join_as(&state, &code, "ben").await;
        let cy = join_as(&state, &code, "cy").await;
        // A name is one person's in a session, whatever its case.
        let resp = join(State(state.clone()), CookieJar::new(), Path(code.clone()), Form(JoinForm { name: "ANA".into(), mode: "guest".into() }))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        // The cookie remembers who you are.
        assert!(page_for(&state, &code, Some(&ana)).await.contains("ana"));
        assert!(page_for(&state, &code, None).await.contains("play as a guest"));

        assert_eq!(guess_as(&state, &code, &ana, "1").await, StatusCode::SEE_OTHER);
        assert_eq!(guess_as(&state, &code, &ben, "1").await, StatusCode::SEE_OTHER);
        assert_eq!(guess_as(&state, &code, &cy, "3").await, StatusCode::SEE_OTHER);
        assert_eq!(guess_as(&state, &code, &cy, "0").await, StatusCode::SEE_OTHER, "already in: ignored");
        let html = page_for(&state, &code, Some(&cy)).await;
        assert!(html.contains("you&#x27;re in") || html.contains("you're in"));
        // Numbers stay secret while it is open.
        assert!(!page_for(&state, &code, None).await.contains("won"));

        let session = db::custom_session(&state.db, &code).await.unwrap().unwrap();
        let resp = end(State(state.clone()), jar("mod"), Path(code.clone())).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert!(!db::custom_session(&state.db, &code).await.unwrap().unwrap().is_open(Utc::now()));
        assert!(links(&state, sent(&[(PLAYER_COOKIE, &ana)])).await.unwrap().is_empty());

        let (rows, winner) = picks(db::custom_picks(&state.db, session.id).await.unwrap(), Some("cy"));
        let w = &rows[winner.unwrap()];
        assert_eq!((w.name.as_str(), w.value_label.as_str(), w.mine), ("cy", "3", true));
        assert!(page_for(&state, &code, Some(&cy)).await.contains("cy"));

        // Over: nobody joins or guesses any more.
        let late = join(State(state.clone()), CookieJar::new(), Path(code.clone()), Form(JoinForm { name: "dee".into(), mode: "guest".into() }))
            .await
            .unwrap();
        assert_eq!(late.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn a_cookie_counts_in_its_own_session_only() {
        let (_t, state) = setup().await;
        let one = open(&state, "30").await;
        let two = open(&state, "30").await;
        let token = join_as(&state, &one, "ana").await;
        // In the other session, which it never joined, it is nobody.
        let resp = guess(State(state.clone()), sent(&[(PLAYER_COOKIE, &token)]), Path(two.clone()), Form(GuessForm { guess: "4".into() }))
            .await
            .unwrap();
        assert_eq!(location(&resp), format!("/s/{two}"));
        let s = db::custom_session(&state.db, &two).await.unwrap().unwrap();
        assert_eq!(db::custom_counts(&state.db, s.id).await.unwrap(), (0, 0));
    }

    #[tokio::test]
    async fn the_round_page_shows_a_session_only_to_those_in_it() {
        let (_t, state) = setup().await;
        let code = open(&state, "30").await;
        let ana = join_as(&state, &code, "ana").await;
        // No cookie, a signed-in student, the moderator who opened it, a
        // made-up token: no button for any of them.
        assert!(links(&state, CookieJar::new()).await.unwrap().is_empty());
        assert!(links(&state, jar("student")).await.unwrap().is_empty());
        assert!(links(&state, jar("mod")).await.unwrap().is_empty());
        let fake = "x".repeat(32);
        assert!(links(&state, sent(&[(PLAYER_COOKIE, &fake)])).await.unwrap().is_empty());
        assert_eq!(links(&state, sent(&[(PLAYER_COOKIE, &ana)])).await.unwrap().len(), 1);

        // And the round page itself, for the one in it and someone who is not.
        let round = |jar: CookieJar| {
            let state = state.clone();
            async move {
                let q = axum::extract::Query(crate::handlers::IndexQuery { msg: None, n: None });
                body(crate::handlers::global_index(State(state), jar, q).await.unwrap()).await
            }
        };
        assert!(round(sent(&[(PLAYER_COOKIE, &ana)])).await.contains(&format!("href=\"/s/{code}\"")));
        assert!(!round(CookieJar::new()).await.contains("/s/"));
    }

    #[tokio::test]
    async fn a_browser_keeps_its_token_from_session_to_session() {
        let (_t, state) = setup().await;
        let one = open(&state, "30").await;
        let two = open(&state, "60").await;
        let token = join_as(&state, &one, "ana").await;
        let resp = join(State(state.clone()), sent(&[(PLAYER_COOKIE, &token)]), Path(two.clone()), Form(JoinForm { name: "ana".into(), mode: "guest".into() }))
            .await
            .unwrap();
        assert_eq!(set_cookie(&resp, PLAYER_COOKIE).as_deref(), Some(token.as_str()));
        // Both buttons, the one closing first on top.
        let links = links(&state, sent(&[(PLAYER_COOKIE, &token)])).await.unwrap();
        let hrefs: Vec<_> = links.iter().map(|l| l.href.clone()).collect();
        assert_eq!(hrefs, [format!("/s/{one}"), format!("/s/{two}")]);
    }

    #[tokio::test]
    async fn signed_in_players_play_under_their_username() {
        let (_t, state) = setup().await;
        let code = open(&state, "30").await;
        // Signed out: sign in, make an account, or a guest.
        let html = page_for(&state, &code, None).await;
        assert!(html.contains(&format!("href=\"/s/{code}/signin\"")));
        assert!(html.contains(&format!("href=\"/s/{code}/signup\"")));
        assert!(html.contains("play as a guest"));
        // Signed in: their username, or a guest.
        let html = body(page(State(state.clone()), jar("student"), Path(code.clone()), Query(PageQuery::default())).await.unwrap()).await;
        assert!(html.contains("play as other"));
        assert!(html.contains("or play as a guest"));

        let account = || Form(JoinForm { name: "whatever".into(), mode: "account".into() });
        let resp = join(State(state.clone()), jar("student"), Path(code.clone()), account()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        assert!(set_cookie(&resp, PLAYER_COOKIE).is_some());
        // Known by the account in another browser, with no session cookie
        // of its own: the button, and their number to pick.
        assert_eq!(links(&state, jar("student")).await.unwrap().len(), 1);
        let resp = guess(State(state.clone()), jar("student"), Path(code.clone()), Form(GuessForm { guess: "5".into() }))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        // Joining again changes nothing.
        join(State(state.clone()), jar("student"), Path(code.clone()), account()).await.unwrap();
        let guest = join_as(&state, &code, "bea").await;
        guess_as(&state, &code, &guest, "9").await;

        end(State(state.clone()), jar("mod"), Path(code.clone())).await.unwrap();
        let session = db::custom_session(&state.db, &code).await.unwrap().unwrap();
        let rows = db::custom_picks(&state.db, session.id).await.unwrap();
        let got: Vec<_> = rows.iter().map(|r| (r.name.as_str(), r.guess, r.guest)).collect();
        assert_eq!(got, [("other", 5, false), ("bea", 9, true)]);
    }

    #[tokio::test]
    async fn a_guest_cannot_take_a_username() {
        let (_t, state) = setup().await;
        let code = open(&state, "30").await;
        for name in ["other", "MEGARDES"] {
            let resp = join(State(state.clone()), CookieJar::new(), Path(code.clone()), Form(JoinForm { name: name.into(), mode: "guest".into() }))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "{name}");
            // What they typed stays, in the guest form left open.
            let html = body(resp).await;
            assert!(html.contains(&format!("value=\"{name}\"")));
            assert!(html.contains("<details class=\"session-guest\" open>"));
        }
        // Signed out, "play as my account" goes back to the choice.
        let resp = join(State(state.clone()), CookieJar::new(), Path(code.clone()), Form(JoinForm { name: String::new(), mode: "account".into() }))
            .await
            .unwrap();
        assert_eq!(location(&resp), format!("/s/{code}"));
    }

    #[tokio::test]
    async fn signing_in_from_a_session_comes_back_to_it() {
        let (_t, state) = setup().await;
        let code = open(&state, "30").await;
        let resp = signin(State(state.clone()), CookieJar::new(), Path(code.clone())).await.unwrap();
        assert_eq!(location(&resp), "/login");
        let back = set_cookie(&resp, RETURN_COOKIE).unwrap();
        assert_eq!(back, code);
        let resp = signup(State(state.clone()), CookieJar::new(), Path(code.clone())).await.unwrap();
        assert_eq!(location(&resp), "/signup");

        let (account,): (i64,) = sqlx::query_as("SELECT id FROM accounts WHERE username = 'other'").fetch_one(&state.db).await.unwrap();
        let signed_in = |cookie: &str, to: &'static str| {
            let (state, cookie) = (state.clone(), cookie.to_string());
            async move {
                let resp = crate::signin::signed_in_to(&state, sent(&[(RETURN_COOKIE, &cookie)]), account, to).await.unwrap();
                // Used once: the note is cleared on the way.
                assert_eq!(set_cookie(&resp, RETURN_COOKIE).as_deref(), Some(""));
                location(&resp).to_string()
            }
        };
        assert_eq!(signed_in(&back, "/g").await, format!("/s/{code}"));
        // A new account names itself first, then comes back.
        assert_eq!(signed_in(&back, "/welcome").await, format!("/welcome?then=/s/{code}"));
        // Anything but a session's page is not followed.
        assert_eq!(signed_in("https://evil.example", "/g").await, "/g");
        assert_eq!(signed_in("../admin", "/g").await, "/g");
        assert!(is_session_page(&format!("/s/{code}")));
        assert!(!is_session_page(&format!("/s/{code}/x")));
        assert!(!is_session_page("//evil.example"));
    }

    #[tokio::test]
    async fn the_page_works_as_the_round_page_does() {
        let (_t, state) = setup().await;
        let code = open(&state, "30").await;
        let ana = join_as(&state, &code, "ana").await;
        let html = page_for(&state, &code, Some(&ana)).await;
        // The clock and bar over the session's length, and the rules.
        assert!(html.contains("class=\"countdown\" data-opens-at="));
        assert!(html.contains("data-reload"));
        assert!(html.contains("class=\"meter\""));
        assert!(html.contains("once this session."));

        // Locking: to the sealed number, shown and stamped just this once.
        let resp = guess(State(state.clone()), sent(&[(PLAYER_COOKIE, &ana)]), Path(code.clone()), Form(GuessForm { guess: "4".into() }))
            .await
            .unwrap();
        assert_eq!(location(&resp), format!("/s/{code}?msg=saved"));
        let saved = Query(PageQuery { msg: Some("saved".into()) });
        let html = body(page(State(state.clone()), sent(&[(PLAYER_COOKIE, &ana)]), Path(code.clone()), saved).await.unwrap()).await;
        assert!(html.contains("class=\"sealed fresh\""));
        assert!(html.contains("class=\"sealed-value peek\" aria-pressed=\"true\""));
        // Afterwards covered, with the button to show it.
        let html = page_for(&state, &code, Some(&ana)).await;
        assert!(html.contains("class=\"sealed-value peek\" aria-pressed=\"false\""));
        assert!(html.contains("tap to show"));
        assert!(!html.contains("class=\"announce\""));

        // Ended: the result over the page, as after 12:42.
        end(State(state.clone()), jar("mod"), Path(code.clone())).await.unwrap();
        let html = page_for(&state, &code, Some(&ana)).await;
        assert!(html.contains("class=\"announce\""));
        assert!(html.contains("ana had it alone"));
        assert!(!html.contains("class=\"countdown\""));
    }

    #[test]
    fn nobody_wins_when_every_number_collides() {
        let row = |name: &str| db::CustomPickRow { name: name.into(), guess: 2, guest: true };
        let (_, winner) = picks(vec![row("a"), row("b")], None);
        assert_eq!(winner, None);
        assert_eq!(clean_code("ABC234").as_deref(), Some("abc234"));
        assert_eq!(clean_code("abc10o"), None);
    }
}
