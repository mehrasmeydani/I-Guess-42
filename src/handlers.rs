use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use axum_extra::extract::cookie::CookieJar;
use chrono::Utc;
use serde::Deserialize;

use crate::app::{error_page, AppError, AppState};
use crate::auth;
use crate::db::{self, User};
use crate::round::Round;
use crate::templates::{
    self, group_digits, AdminTemplate, ConfirmTemplate, IndexTemplate, Notice, ResultsTemplate,
    RoundView, UserView,
};

const HISTORY_LIMIT: i64 = 60;
const LEADERBOARD_LIMIT: i64 = 20;

async fn current_user(state: &AppState, jar: &CookieJar) -> Result<Option<User>, AppError> {
    let Some(token) = auth::session_token(jar) else {
        return Ok(None);
    };
    Ok(db::user_for_session(&state.db, &token).await?)
}

/// Flash messages travel as a fixed code in the query string, so nothing a
/// visitor types is ever echoed back into the page.
fn notice_for(code: Option<&str>) -> Option<Notice> {
    Some(match code? {
        "saved" => Notice::ok("Locked in. Come back after 12:42 to see who took it."),
        "added" => Notice::ok("Added, and it counts towards the round."),
        "added_ghost" => Notice::ok("Added as a ghost — visible here, but it cannot win."),
        "cleared" => Notice::ok("Round cleared."),
        "not_demo" => {
            Notice::error("That is a real 42 account. Only stand-in players can be signed in as.")
        }
        "bad_login" => Notice::error("Logins may only contain letters, digits and hyphens."),
        "bad_guess" => Notice::error("That is not a whole number of 1 or more."),
        "already" => {
            Notice::error("You have already guessed this round, and a guess cannot be changed.")
        }
        "rolled" => Notice::error(
            "The round closed while you were confirming, so nothing was submitted. \
             The next round is open now.",
        ),
        "empty" => Notice::error("Type a number first."),
        "invalid" => Notice::error("Whole numbers only — no signs, decimals or letters."),
        "too_small" => Notice::error("The smallest allowed guess is 1."),
        "too_big" => Notice::error(
            "That is past the ceiling of 9 223 372 036 854 775 807. Aim lower — that is the point.",
        ),
        "login_required" => Notice::error("Sign in with 42 before guessing."),
        _ => return None,
    })
}

#[derive(Deserialize)]
pub struct IndexQuery {
    msg: Option<String>,
}

pub async fn index(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<IndexQuery>,
) -> Result<Response, AppError> {
    let now = state.now();
    let round = Round::current(now);
    let round_key = round.key();

    let user = current_user(&state, &jar).await?;
    let my_guess_label = match &user {
        Some(u) => db::my_guess(&state.db, &round_key, u.id)
            .await?
            .map(group_digits),
        None => None,
    };

    let last_round = db::round_summary(
        &state.db,
        &round_key,
        &round.previous_date().format("%Y-%m-%d").to_string(),
    )
    .await?
    .map(RoundView::from);

    let seconds_left = round.seconds_left(now);
    let guess_count = db::guess_count(&state.db, &round_key).await?;
    let round_label = crate::round::format_round_date(&round_key);

    Ok(templates::render(&IndexTemplate {
        user: user.map(UserView::from),
        test_mode: state.cfg.test_mode(),
        impersonating: auth::admin_return_token(&jar).is_some(),
        round_key,
        round_label,
        deadline_human: round.deadline_human(),
        seconds_left,
        time_left: crate::round::format_duration(seconds_left),
        my_guess_label,
        guess_count,
        last_round,
        notice: notice_for(query.msg.as_deref()),
    }))
}

pub async fn results(State(state): State<AppState>, jar: CookieJar) -> Result<Response, AppError> {
    let round_key = Round::current(state.now()).key();
    let user = current_user(&state, &jar).await?;

    let rounds = db::closed_rounds(&state.db, &round_key, HISTORY_LIMIT)
        .await?
        .into_iter()
        .map(RoundView::from)
        .collect();
    let leaders = db::leaderboard(&state.db, &round_key, LEADERBOARD_LIMIT)
        .await?
        .into_iter()
        .map(Into::into)
        .collect();

    Ok(templates::render(&ResultsTemplate {
        user: user.map(UserView::from),
        test_mode: state.cfg.test_mode(),
        impersonating: auth::admin_return_token(&jar).is_some(),
        rounds,
        leaders,
    }))
}

// ---------------------------------------------------------------- guessing

#[derive(Deserialize)]
pub struct GuessForm {
    guess: String,
    /// The round the form was rendered for, so we can tell the player when
    /// their number landed in a different one than they were looking at.
    round: String,
}

/// Accepts digits with the separators people naturally type.
fn parse_guess(raw: &str) -> Result<i64, &'static str> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '_' && *c != ',' && *c != '\u{202f}')
        .collect();

    if cleaned.is_empty() {
        return Err("empty");
    }
    if !cleaned.chars().all(|c| c.is_ascii_digit()) {
        return Err("invalid");
    }
    match cleaned.parse::<i64>() {
        Ok(0) => Err("too_small"),
        Ok(n) => Ok(n),
        Err(_) => Err("too_big"), // all digits, so the only way to fail is overflow
    }
}

/// Step one: validate the number and show it back for confirmation. Nothing
/// is written here — a guess cannot be taken back, so it is worth an extra
/// click. The confirmation is a real page rather than a JavaScript dialog, so
/// it still works with scripting turned off.
pub async fn submit_guess(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<GuessForm>,
) -> Result<Response, AppError> {
    let Some(user) = current_user(&state, &jar).await? else {
        return Ok(Redirect::to("/?msg=login_required").into_response());
    };

    let value = match parse_guess(&form.guess) {
        Ok(v) => v,
        Err(code) => return Ok(Redirect::to(&format!("/?msg={code}")).into_response()),
    };

    let round = Round::current(state.now());
    let round_key = round.key();

    if db::my_guess(&state.db, &round_key, user.id)
        .await?
        .is_some()
    {
        return Ok(Redirect::to("/?msg=already").into_response());
    }

    Ok(templates::render(&ConfirmTemplate {
        user: Some(UserView::from(user)),
        test_mode: state.cfg.test_mode(),
        impersonating: auth::admin_return_token(&jar).is_some(),
        value_label: group_digits(value),
        // Canonical form, so what gets stored is exactly what was shown:
        // "007" was displayed as 7 and must be submitted as 7.
        value_raw: value.to_string(),
        round_key,
        round_label: crate::round::format_round_date(&round.key()),
        deadline_human: round.deadline_human(),
    }))
}

/// Step two: actually record it.
pub async fn confirm_guess(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<GuessForm>,
) -> Result<Response, AppError> {
    let Some(user) = current_user(&state, &jar).await? else {
        return Ok(Redirect::to("/?msg=login_required").into_response());
    };

    let value = match parse_guess(&form.guess) {
        Ok(v) => v,
        Err(code) => return Ok(Redirect::to(&format!("/?msg={code}")).into_response()),
    };

    // If the deadline passed between the two steps, refuse rather than
    // quietly committing an irreversible guess to a round they never saw.
    let round_key = Round::current(state.now()).key();
    if form.round != round_key {
        return Ok(Redirect::to("/?msg=rolled").into_response());
    }

    let msg = if db::insert_guess(&state.db, &round_key, user.id, value, true).await? {
        "saved"
    } else {
        // Lost a race with another tab; the first guess stands.
        "already"
    };
    Ok(Redirect::to(&format!("/?msg={msg}")).into_response())
}

// ------------------------------------------------------------------- admin
//
// Test-instance tooling: shift the clock past a deadline, invent players, and
// look at an open round. All of it is gated on ADMIN_LOGINS being set, and
// every route answers 404 when it is not, so a live deployment gives no sign
// that any of this exists.

/// `Ok(Some(user))` for a signed-in admin on a test instance, `Ok(None)`
/// otherwise - callers turn that into a 404.
async fn require_admin(state: &AppState, jar: &CookieJar) -> Result<Option<User>, AppError> {
    if !state.cfg.test_mode() {
        return Ok(None);
    }
    Ok(current_user(state, jar)
        .await?
        .filter(|u| state.cfg.is_admin(&u.login)))
}

fn admin_gone() -> Response {
    error_page(StatusCode::NOT_FOUND, "No such page.")
}

pub async fn admin(State(state): State<AppState>, jar: CookieJar) -> Result<Response, AppError> {
    let Some(user) = require_admin(&state, &jar).await? else {
        return Ok(admin_gone());
    };

    let now = state.now();
    let round = Round::current(now);
    let round_key = round.key();
    let offset = state.clock_offset();

    let last_round = db::round_summary(
        &state.db,
        &round_key,
        &round.previous_date().format("%Y-%m-%d").to_string(),
    )
    .await?
    .map(RoundView::from);

    Ok(templates::render(&AdminTemplate {
        user: Some(UserView::from(user)),
        test_mode: true,
        impersonating: auth::admin_return_token(&jar).is_some(),
        real_now: Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        game_now: now.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        clock_offset: if offset == 0 {
            "none".to_string()
        } else {
            format!(
                "{:+} ({})",
                offset,
                crate::round::format_duration(offset.abs())
            )
        },
        round_key: round_key.clone(),
        round_label: crate::round::format_round_date(&round_key),
        deadline_human: round.deadline_human(),
        time_left: crate::round::format_duration(round.seconds_left(now)),
        guesses: db::round_guesses(&state.db, &round_key)
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
        demo_users: db::demo_users(&state.db)
            .await?
            .into_iter()
            .map(UserView::from)
            .collect(),
        last_round,
    }))
}

#[derive(Deserialize)]
pub struct ClockForm {
    shift: String,
}

pub async fn admin_clock(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<ClockForm>,
) -> Result<Response, AppError> {
    if require_admin(&state, &jar).await?.is_none() {
        return Ok(admin_gone());
    }

    match form.shift.as_str() {
        "reset" => state.reset_clock(),
        // Land one second past the deadline, so the round is definitively over.
        "deadline" => {
            let now = state.now();
            state.shift_clock(Round::current(now).seconds_left(now) + 1);
        }
        "hour" => state.shift_clock(60 * 60),
        "day" => state.shift_clock(24 * 60 * 60),
        "back_day" => state.shift_clock(-24 * 60 * 60),
        _ => return Ok(Redirect::to("/admin?msg=invalid").into_response()),
    }
    Ok(Redirect::to("/admin").into_response())
}

#[derive(Deserialize)]
pub struct FakeGuessForm {
    login: String,
    guess: String,
    /// Present only when the checkbox is ticked; HTML omits unchecked boxes.
    #[serde(default)]
    ghost: Option<String>,
}

/// Adds a guess on behalf of an invented player, so a round can be populated
/// without recruiting people.
pub async fn admin_fake_guess(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<FakeGuessForm>,
) -> Result<Response, AppError> {
    if require_admin(&state, &jar).await?.is_none() {
        return Ok(admin_gone());
    }

    let login = form.login.trim().to_lowercase();
    if login.is_empty() || !login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Ok(Redirect::to("/admin?msg=bad_login").into_response());
    }
    let Ok(value) = parse_guess(&form.guess) else {
        return Ok(Redirect::to("/admin?msg=bad_guess").into_response());
    };

    let player = match db::user_by_login(&state.db, &login).await? {
        Some(u) => u,
        None => db::create_test_user(&state.db, &login).await?,
    };

    let participates = form.ghost.is_none();
    let round_key = Round::current(state.now()).key();
    let msg = if db::insert_guess(&state.db, &round_key, player.id, value, participates).await? {
        if participates {
            "added"
        } else {
            "added_ghost"
        }
    } else {
        "already"
    };
    Ok(Redirect::to(&format!("/admin?msg={msg}")).into_response())
}

#[derive(Deserialize)]
pub struct ImpersonateForm {
    login: String,
}

/// Signs the admin in as a demo account so they can play through the real
/// flow. Only negative-id stand-ins can be impersonated - never a genuine 42
/// account, which matters because real classmates can sign in to a test
/// instance too.
pub async fn admin_impersonate(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<ImpersonateForm>,
) -> Result<Response, AppError> {
    if require_admin(&state, &jar).await?.is_none() {
        return Ok(admin_gone());
    }

    let login = form.login.trim().to_lowercase();
    if login.is_empty() || !login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Ok(Redirect::to("/admin?msg=bad_login").into_response());
    }

    let player = match db::user_by_login(&state.db, &login).await? {
        Some(u) if u.id < 0 => u,
        // Refuse to become a real person, even on a test box.
        Some(_) => return Ok(Redirect::to("/admin?msg=not_demo").into_response()),
        None => db::create_test_user(&state.db, &login).await?,
    };

    let token = auth::random_token();
    db::create_session(&state.db, &token, player.id, auth::SESSION_TTL_DAYS).await?;

    // Park the admin's own session so they can come back. Keep whichever one
    // is already parked, so impersonating twice does not lose the way home.
    let mut jar = jar;
    if auth::admin_return_token(&jar).is_none() {
        if let Some(mine) = auth::session_token(&jar) {
            jar = jar.add(auth::admin_return_cookie(mine, state.cfg.secure_cookies));
        }
    }
    let jar = jar.add(auth::session_cookie(token, state.cfg.secure_cookies));
    Ok((jar, Redirect::to("/")).into_response())
}

/// Swaps back to the parked admin session. Deliberately does NOT call
/// require_admin: the caller is currently a demo account, so that check would
/// 404 and strand them.
pub async fn admin_return(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response, AppError> {
    if !state.cfg.test_mode() {
        return Ok(admin_gone());
    }
    let Some(parked) = auth::admin_return_token(&jar) else {
        return Ok(Redirect::to("/").into_response());
    };

    // Only restore a session that is still valid and still belongs to an admin.
    let restores_admin = db::user_for_session(&state.db, &parked)
        .await?
        .is_some_and(|u| state.cfg.is_admin(&u.login));

    let jar = jar.remove(auth::clearing_admin_return_cookie());
    if !restores_admin {
        return Ok((jar, Redirect::to("/")).into_response());
    }

    // Drop the throwaway demo session rather than leaving it lying around.
    if let Some(demo) = auth::session_token(&jar) {
        db::delete_session(&state.db, &demo).await?;
    }
    let jar = jar.add(auth::session_cookie(parked, state.cfg.secure_cookies));
    Ok((jar, Redirect::to("/admin")).into_response())
}

pub async fn admin_clear(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response, AppError> {
    if require_admin(&state, &jar).await?.is_none() {
        return Ok(admin_gone());
    }
    let round_key = Round::current(state.now()).key();
    db::clear_round(&state.db, &round_key).await?;
    Ok(Redirect::to("/admin?msg=cleared").into_response())
}

// -------------------------------------------------------------------- auth

pub async fn login(State(state): State<AppState>) -> Result<Response, AppError> {
    let state_token = auth::random_token();
    db::store_oauth_state(&state.db, &state_token).await?;
    let url = auth::authorize_url(&state.cfg, &state_token)?;
    Ok(Redirect::to(&url).into_response())
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

pub async fn callback(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<CallbackQuery>,
) -> Result<Response, AppError> {
    if let Some(err) = query.error {
        let detail = query.error_description.unwrap_or(err);
        // Askama escapes this on the way into the page.
        return Ok(error_page(
            StatusCode::BAD_REQUEST,
            &format!("42 refused the sign-in: {detail}"),
        ));
    }

    let (Some(code), Some(state_token)) = (query.code, query.state) else {
        return Ok(error_page(
            StatusCode::BAD_REQUEST,
            "That sign-in link is incomplete. Start again from the home page.",
        ));
    };

    if !db::consume_oauth_state(&state.db, &state_token).await? {
        return Ok(error_page(
            StatusCode::BAD_REQUEST,
            "That sign-in link has expired or was already used. Start again from the home page.",
        ));
    }

    let access_token = auth::exchange_code(&state.http, &state.cfg, &code).await?;
    let me = auth::fetch_me(&state.http, &access_token).await?;

    db::upsert_user(
        &state.db,
        me.id,
        &me.login,
        me.display_name(),
        me.image_url(),
    )
    .await?;

    let session = auth::random_token();
    db::create_session(&state.db, &session, me.id, auth::SESSION_TTL_DAYS).await?;

    let jar = jar.add(auth::session_cookie(session, state.cfg.secure_cookies));
    Ok((jar, Redirect::to("/")).into_response())
}

pub async fn logout(State(state): State<AppState>, jar: CookieJar) -> Result<Response, AppError> {
    if let Some(token) = auth::session_token(&jar) {
        db::delete_session(&state.db, &token).await?;
    }
    let jar = jar.remove(auth::clearing_cookie());
    Ok((jar, Redirect::to("/")).into_response())
}

/// Liveness probe for load balancers and `docker compose` healthchecks.
/// Touches the database so a wedged pool reports unhealthy rather than OK.
pub async fn healthz(State(state): State<AppState>) -> Response {
    match sqlx::query("SELECT 1").execute(&state.db).await {
        Ok(_) => (StatusCode::OK, "ok").into_response(),
        Err(err) => {
            tracing::error!(%err, "health check failed");
            (StatusCode::SERVICE_UNAVAILABLE, "database unavailable").into_response()
        }
    }
}

pub async fn not_found() -> Response {
    error_page(StatusCode::NOT_FOUND, "No such page.")
}

#[cfg(test)]
mod tests {
    use super::parse_guess;

    #[test]
    fn accepts_plain_and_separated_digits() {
        assert_eq!(parse_guess("7"), Ok(7));
        assert_eq!(parse_guess("  42 "), Ok(42));
        assert_eq!(parse_guess("1,234"), Ok(1234));
        assert_eq!(parse_guess("1 000 000"), Ok(1_000_000));
        assert_eq!(parse_guess("007"), Ok(7));
        assert_eq!(parse_guess("9223372036854775807"), Ok(i64::MAX));
    }

    #[test]
    fn rejects_everything_that_is_not_a_positive_whole_number() {
        assert_eq!(parse_guess(""), Err("empty"));
        assert_eq!(parse_guess("   "), Err("empty"));
        assert_eq!(parse_guess("-5"), Err("invalid"));
        assert_eq!(parse_guess("3.5"), Err("invalid"));
        assert_eq!(parse_guess("twelve"), Err("invalid"));
        assert_eq!(parse_guess("1e9"), Err("invalid"));
        assert_eq!(parse_guess("0"), Err("too_small"));
        assert_eq!(parse_guess("0000"), Err("too_small"));
        assert_eq!(parse_guess("9223372036854775808"), Err("too_big"));
    }
}
