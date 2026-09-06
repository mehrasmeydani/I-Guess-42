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
    self, group_digits, IndexTemplate, Notice, ResultsTemplate, RoundView, UserView,
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
        "changed" => Notice::ok("Guess updated — the new number is the one that counts."),
        "rolled" => Notice::error(
            "The round closed while you were deciding, so your number went into the next one.",
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
    let now = Utc::now();
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
    let round_key = Round::current(Utc::now()).key();
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

    // Recomputed here rather than trusted from the form: the guess always
    // lands in whichever round is open at this instant.
    let round_key = Round::current(Utc::now()).key();
    let had_guess = db::my_guess(&state.db, &round_key, user.id)
        .await?
        .is_some();
    db::upsert_guess(&state.db, &round_key, user.id, value).await?;

    let msg = if form.round != round_key {
        "rolled"
    } else if had_guess {
        "changed"
    } else {
        "saved"
    };
    Ok(Redirect::to(&format!("/?msg={msg}")).into_response())
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
