use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use axum_extra::extract::cookie::CookieJar;
use chrono::Utc;
use serde::Deserialize;

use crate::app::{error_page, AppError, AppState};
use crate::auth;
use crate::db::{self, Pool, Viewer};
use crate::round::Round;
use crate::stats;
use crate::templates::{
    self, group_digits, AdminTemplate, ConfirmTemplate, DayTemplate, IndexTemplate, Notice,
    ResultsTemplate, RoundView, TrendsTemplate, UserView,
};

/// Rounds are loaded in full and narrowed down in Rust: search and the
/// 7 / 30 / all switch work on the whole history.
const HISTORY_LIMIT: i64 = i64::MAX;
const LEADERBOARD_LIMIT: i64 = i64::MAX;
/// Shown up front on the results page; the rest of the leaderboard folds away.
const PODIUM: usize = 3;
/// How many earlier rounds a day page lists under itself. A fortnight is
/// enough to find your way around without turning the page into /results.
const DAY_HISTORY: i64 = 14;

pub(crate) async fn current_viewer(state: &AppState, jar: &CookieJar) -> Result<Option<Viewer>, AppError> {
    let Some(token) = auth::session_token(jar) else {
        return Ok(None);
    };
    Ok(db::viewer_for_session(&state.db, &token).await?)
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
        "not_42" => Notice::error("This round is for 42 Vienna students: sign in with 42 to play it."),
        "signin" => Notice::error("Sign in before guessing."),
        "verified" => Notice::ok("Email confirmed. You're in: pick a number."),
        "password_set" => Notice::ok("New password saved, and every other session signed out."),
        "resent" => Notice::ok("Sent another link. It works for 24 hours."),
        "resend_wait" => Notice::error("We sent a few links already. Give it an hour."),
        "mail_failed" => Notice::error("The mail didn't go out. Try again later."),
        "welcome" => Notice::ok("Welcome. Pick a number: lowest one nobody else picks wins."),
        "deleted" => Notice::ok("Your account is deleted."),
        "unverified" => Notice::error(
            "Verify your email address to play: open the link we sent you.",
        ),
        _ => return None,
    })
}

/// Whether this visitor may be shown who played in `pool`.
///
/// In the 42 pool a login and a real name are a 42 student's personal data,
/// so they leave the server only for a visitor who plays as a 42 student;
/// being signed in is not enough, since an account need not be a 42 one.
/// In the global pool everyone goes by a username they chose, shown to anyone
/// signed in. Everyone else gets `templates::MASK`.
fn names_visible(pool: Pool, viewer: Option<&Viewer>) -> bool {
    match pool {
        Pool::Vienna => viewer.is_some_and(|v| v.intra.is_some()),
        Pool::Global => viewer.is_some(),
    }
}

/// The id this visitor's guesses in `pool` are stored under, or `None` if
/// they may look at the pool but not play it: the 42 pool wants a 42 player
/// (intra id), the global one a verified account (account id).
fn player_in(pool: Pool, viewer: &Viewer) -> Option<i64> {
    match pool {
        Pool::Vienna => viewer.intra.as_ref().map(|u| u.id),
        Pool::Global => viewer.account.verified.then_some(viewer.account.id),
    }
}

/// Where a pool's front page lives, for redirects back to it.
fn home(pool: Pool) -> &'static str {
    match pool {
        Pool::Vienna => "/",
        Pool::Global => "/g",
    }
}

/// Why `viewer` cannot play `pool`, as a notice code for its front page.
fn refusal(pool: Pool) -> &'static str {
    match pool {
        Pool::Vienna => "not_42",
        Pool::Global => "unverified",
    }
}

#[derive(Deserialize)]
pub struct IndexQuery {
    pub msg: Option<String>,
}

pub async fn index(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<IndexQuery>,
) -> Result<Response, AppError> {
    index_in(Pool::Vienna, state, jar, query).await
}

pub async fn global_index(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<IndexQuery>,
) -> Result<Response, AppError> {
    index_in(Pool::Global, state, jar, query).await
}

async fn index_in(
    pool: Pool,
    state: AppState,
    jar: CookieJar,
    query: IndexQuery,
) -> Result<Response, AppError> {
    let now = state.now();
    let round = Round::current(now);
    let round_key = round.key();

    let viewer = current_viewer(&state, &jar).await?;
    let player = viewer.as_ref().and_then(|v| player_in(pool, v));
    let my_guess_label = match player {
        Some(p) => db::my_guess(&state.db, pool, &round_key, p)
            .await?
            .map(group_digits),
        None => None,
    };

    let last_round = db::round_summary(
        &state.db,
        pool,
        &round_key,
        &round.previous_date().format("%Y-%m-%d").to_string(),
    )
    .await?
    .map(|r| RoundView::new(r, names_visible(pool, viewer.as_ref())));

    // For half an hour after 12:42 the round that just closed is announced
    // over the page: it is the one moment the game has, and without this it
    // goes by unnoticed. Only ever the closed round -- guesses are secret
    // until the deadline and this is the first thing to say them out loud.
    const ANNOUNCE_FOR: i64 = 30 * 60;
    let announce_left = Some(ANNOUNCE_FOR - round.since_previous_close(now))
        .filter(|left| *left > 0 && last_round.is_some());

    // The points line is for players, not for the shop window: it says what
    // this round is worth to *you*, so it waits until there is a you. Only
    // the 42 pool pays coalition points.
    let points_announced =
        pool == Pool::Vienna && player.is_some() && state.cfg.points_announced();

    let seconds_left = round.seconds_left(now);
    let (progress_pct, progress_bar) = stats::progress_bar(seconds_left);
    let guess_count = db::guess_count(&state.db, pool, &round_key).await?;
    Ok(templates::render(&IndexTemplate {
        pool,
        can_play: player.is_some(),
        user: viewer.map(UserView::from),
        test_mode: state.cfg.test_mode(),
        impersonating: auth::admin_return_token(&jar).is_some(),
        round_key,
        seconds_left,
        time_left: crate::round::format_duration(seconds_left),
        progress_pct,
        progress_bar,
        my_guess_label,
        guess_count,
        last_round,
        announce_left,
        points_announced,
        notice: notice_for(query.msg.as_deref()),
    }))
}

#[derive(Deserialize)]
pub struct ResultsQuery {
    /// Search: a winner's login, a winning number, or a date or month prefix.
    pub q: Option<String>,
    /// "7" (default), "30" or "all": how many recent rounds to list.
    pub show: Option<String>,
    /// From the date picker: jump straight to that day's page.
    pub date: Option<String>,
}

/// Whether a closed round matches a search. The query is matched against the
/// date as a prefix (`2026-08` finds August), the winner's login as a
/// substring, and the winning number exactly (separators allowed).
///
/// `names_visible` also governs searching, not just display: a visitor who may
/// not see logins may not search them either. A match would otherwise answer
/// "which rounds did this student win?" for anyone who can guess a login, with
/// the name masked in a result that only exists because the name matched.
fn round_matches(r: &db::RoundSummary, query: &str, names_visible: bool) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    if r.round_date.starts_with(&q) {
        return true;
    }
    if names_visible && r.winner_login.as_deref().is_some_and(|l| l.to_lowercase().contains(&q)) {
        return true;
    }
    matches!((parse_guess(&q), r.winning_value), (Ok(n), Some(v)) if n == v)
}

pub async fn results(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<ResultsQuery>,
) -> Result<Response, AppError> {
    results_in(Pool::Vienna, state, jar, query).await
}

pub async fn global_results(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<ResultsQuery>,
) -> Result<Response, AppError> {
    results_in(Pool::Global, state, jar, query).await
}

async fn results_in(
    pool: Pool,
    state: AppState,
    jar: CookieJar,
    query: ResultsQuery,
) -> Result<Response, AppError> {
    if let Some(date) = query.date.as_deref().filter(|d| !d.is_empty()) {
        // Only a well-formed date is echoed into a URL.
        if chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok() {
            return Ok(Redirect::to(&format!("{}/day/{date}", pool.base())).into_response());
        }
    }

    let round_key = Round::current(state.now()).key();
    let viewer = current_viewer(&state, &jar).await?;

    let named = names_visible(pool, viewer.as_ref());

    let all = db::closed_rounds(&state.db, pool, &round_key, HISTORY_LIMIT).await?;
    let total_rounds = all.len();

    // The round that closed most recently, with the same chart the day page
    // draws. It is what most people open /results for, so it sits above the
    // leaderboard and stays there whatever the range switch or a search does
    // to the table below. One extra day's tallies; the page is already doing
    // more work than this.
    let (latest, chart) = match all.first().cloned() {
        Some(newest) => {
            let tallies = db::round_tallies(&state.db, pool, &newest.round_date).await?;
            let day = stats::analyse(&tallies);
            let chart = stats::day_chart(&tallies, &day);
            (Some(RoundView::new(newest, named)), Some(chart))
        }
        None => (None, None),
    };
    let q = query.q.unwrap_or_default().trim().to_string();
    let show: &'static str = match query.show.as_deref() {
        Some("30") => "30",
        Some("all") => "all",
        _ => "7",
    };
    // A search looks through everything; otherwise only the most recent few.
    let limit = match (q.is_empty(), show) {
        (false, _) | (true, "all") => usize::MAX,
        (true, "30") => 30,
        _ => 7,
    };
    let rounds: Vec<RoundView> = all
        .into_iter()
        .filter(|r| round_matches(r, &q, named))
        .take(limit)
        .map(|r| RoundView::new(r, named))
        .collect();

    let mut leaders: Vec<templates::LeaderView> = db::leaderboard(&state.db, pool, &round_key, LEADERBOARD_LIMIT)
        .await?
        .into_iter()
        .map(|r| templates::LeaderView::new(r, named))
        .collect();
    let rest = leaders.split_off(leaders.len().min(PODIUM));

    Ok(templates::render(&ResultsTemplate {
        pool,
        user: viewer.map(UserView::from),
        test_mode: state.cfg.test_mode(),
        impersonating: auth::admin_return_token(&jar).is_some(),
        latest,
        chart,
        rounds,
        total_rounds,
        q,
        show,
        names_visible: named,
        leaders,
        rest,
    }))
}

/// One closed round in detail: how the numbers were spread, who won, and
/// which numbers were crowded or left alone.
pub async fn day(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(date): Path<String>,
) -> Result<Response, AppError> {
    day_in(Pool::Vienna, state, jar, date).await
}

pub async fn global_day(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(date): Path<String>,
) -> Result<Response, AppError> {
    day_in(Pool::Global, state, jar, date).await
}

async fn day_in(
    pool: Pool,
    state: AppState,
    jar: CookieJar,
    date: String,
) -> Result<Response, AppError> {
    let no_such_day = || error_page(StatusCode::NOT_FOUND, "No closed round on that day.");

    // Only well-formed dates reach the database.
    let is_date = chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d")
        .is_ok_and(|d| d.format("%Y-%m-%d").to_string() == date);
    if !is_date {
        return Ok(no_such_day());
    }

    // round_summary only answers for closed rounds, which is what keeps the
    // open round's numbers secret: no summary, no tallies.
    let open_round = Round::current(state.now()).key();
    let Some(summary) = db::round_summary(&state.db, pool, &open_round, &date).await? else {
        return Ok(no_such_day());
    };

    let tallies = db::round_tallies(&state.db, pool, &date).await?;
    let stats = stats::analyse(&tallies);
    let chart = stats::day_chart(&tallies, &stats);
    let viewer = current_viewer(&state, &jar).await?;
    let named = names_visible(pool, viewer.as_ref());

    // The neighbouring rounds, so one day leads to the next without a detour
    // through /results. Backwards by the listful, forwards one step, which is
    // all it takes to climb back out of the history.
    let earlier: Vec<RoundView> = db::rounds_before(&state.db, pool, &open_round, &date, DAY_HISTORY)
        .await?
        .into_iter()
        .map(|r| RoundView::new(r, named))
        .collect();
    let newer = db::round_after(&state.db, pool, &open_round, &date).await?;
    let total_rounds = db::closed_round_count(&state.db, pool, &open_round).await?;

    Ok(templates::render(&DayTemplate {
        pool,
        user: viewer.map(UserView::from),
        test_mode: state.cfg.test_mode(),
        impersonating: auth::admin_return_token(&jar).is_some(),
        round: RoundView::new(summary, named),
        distinct: stats.distinct,
        lowest_unpicked_label: group_digits(stats.lowest_unpicked),
        most: stats.most.iter().map(Into::into).collect(),
        all: tallies.iter().map(Into::into).collect(),
        chart,
        earlier,
        newer,
        total_rounds,
    }))
}

#[derive(Deserialize)]
pub struct TrendsQuery {
    days: Option<String>,
}

/// Several closed rounds taken together: the last 7 or 30 days, or all of them.
pub async fn trends(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<TrendsQuery>,
) -> Result<Response, AppError> {
    trends_in(Pool::Vienna, state, jar, query).await
}

pub async fn global_trends(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<TrendsQuery>,
) -> Result<Response, AppError> {
    trends_in(Pool::Global, state, jar, query).await
}

async fn trends_in(
    pool: Pool,
    state: AppState,
    jar: CookieJar,
    query: TrendsQuery,
) -> Result<Response, AppError> {
    let (range, days) = match query.days.as_deref() {
        Some("30") => ("30", Some(30)),
        Some("all") => ("all", None),
        _ => ("7", Some(7)),
    };

    let round = Round::current(state.now());
    let open_round = round.key();
    // The range counts back from the most recent closed round.
    let from = match days {
        Some(n) => (round.previous_date() - chrono::Duration::days(n - 1))
            .format("%Y-%m-%d")
            .to_string(),
        None => String::new(), // sorts before every date
    };

    let viewer = current_viewer(&state, &jar).await?;

    // Timed from the query to the finished view model, and shown on the page:
    // an honest number for how heavy the analysis is.
    let started = std::time::Instant::now();
    let rows = db::range_tallies(&state.db, pool, &from, &open_round).await?;
    let trend = stats::trend(&rows);
    let mut page = TrendsTemplate::build(
        pool,
        viewer.map(UserView::from),
        state.cfg.test_mode(),
        auth::admin_return_token(&jar).is_some(),
        range,
        &trend,
    );
    page.took = format!("{:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
    Ok(templates::render(&page))
}

// ---------------------------------------------------------------- guessing

#[derive(Deserialize)]
pub struct GuessForm {
    pub guess: String,
    /// The round the form was rendered for, so we can tell the player when
    /// their number landed in a different one than they were looking at.
    pub round: String,
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
    submit_guess_in(Pool::Vienna, state, jar, form).await
}

pub async fn global_submit_guess(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<GuessForm>,
) -> Result<Response, AppError> {
    submit_guess_in(Pool::Global, state, jar, form).await
}

/// Back to the pool's front page with a notice code.
fn back(pool: Pool, code: &str) -> Response {
    Redirect::to(&format!("{}?msg={code}", home(pool))).into_response()
}

/// The signed-in visitor and the id they play `pool` as, or the redirect
/// that turns them away: not signed in, or not allowed in this pool.
async fn player_or_back(
    pool: Pool,
    state: &AppState,
    jar: &CookieJar,
) -> Result<Result<(Viewer, i64), Response>, AppError> {
    let Some(viewer) = current_viewer(state, jar).await? else {
        let code = match pool {
            Pool::Vienna => "login_required",
            Pool::Global => "signin",
        };
        return Ok(Err(back(pool, code)));
    };
    match player_in(pool, &viewer) {
        Some(player) => Ok(Ok((viewer, player))),
        None => Ok(Err(back(pool, refusal(pool)))),
    }
}

async fn submit_guess_in(
    pool: Pool,
    state: AppState,
    jar: CookieJar,
    form: GuessForm,
) -> Result<Response, AppError> {
    let (viewer, player) = match player_or_back(pool, &state, &jar).await? {
        Ok(found) => found,
        Err(away) => return Ok(away),
    };

    let value = match parse_guess(&form.guess) {
        Ok(v) => v,
        Err(code) => return Ok(back(pool, code)),
    };

    let round = Round::current(state.now());
    let round_key = round.key();

    if db::my_guess(&state.db, pool, &round_key, player)
        .await?
        .is_some()
    {
        return Ok(back(pool, "already"));
    }

    Ok(templates::render(&ConfirmTemplate {
        pool,
        user: Some(UserView::from(viewer)),
        test_mode: state.cfg.test_mode(),
        impersonating: auth::admin_return_token(&jar).is_some(),
        value_label: group_digits(value),
        // Canonical form, so what gets stored is exactly what was shown:
        // "007" was displayed as 7 and must be submitted as 7.
        value_raw: value.to_string(),
        round_key,
        deadline_human: round.deadline_human(),
    }))
}

/// Step two: actually record it.
pub async fn confirm_guess(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<GuessForm>,
) -> Result<Response, AppError> {
    confirm_guess_in(Pool::Vienna, state, jar, form).await
}

pub async fn global_confirm_guess(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<GuessForm>,
) -> Result<Response, AppError> {
    confirm_guess_in(Pool::Global, state, jar, form).await
}

async fn confirm_guess_in(
    pool: Pool,
    state: AppState,
    jar: CookieJar,
    form: GuessForm,
) -> Result<Response, AppError> {
    let player = match player_or_back(pool, &state, &jar).await? {
        Ok((_, player)) => player,
        Err(away) => return Ok(away),
    };

    let value = match parse_guess(&form.guess) {
        Ok(v) => v,
        Err(code) => return Ok(back(pool, code)),
    };

    // If the deadline passed between the two steps, refuse rather than
    // quietly committing an irreversible guess to a round they never saw.
    let round_key = Round::current(state.now()).key();
    if form.round != round_key {
        return Ok(back(pool, "rolled"));
    }

    let msg = if db::insert_guess(&state.db, pool, &round_key, player, value, true).await? {
        "saved"
    } else {
        // Lost a race with another tab; the first guess stands.
        "already"
    };
    Ok(back(pool, msg))
}

// ------------------------------------------------------------------- admin
//
// Test-instance tooling: shift the clock past a deadline, invent players, and
// look at an open round. All of it is gated on ADMIN_LOGINS being set, and
// every route answers 404 when it is not, so a live deployment gives no sign
// that any of this exists.

/// `Ok(Some(viewer))` for a signed-in admin on a test instance, `Ok(None)`
/// otherwise - callers turn that into a 404. ADMIN_LOGINS are 42 logins, so
/// only an account that plays as a 42 student can be one.
pub(crate) async fn require_admin(state: &AppState, jar: &CookieJar) -> Result<Option<Viewer>, AppError> {
    if !state.cfg.test_mode() {
        return Ok(None);
    }
    Ok(current_viewer(state, jar)
        .await?
        .filter(|v| is_admin(state, v)))
}

fn is_admin(state: &AppState, viewer: &Viewer) -> bool {
    viewer
        .intra
        .as_ref()
        .is_some_and(|u| state.cfg.is_admin(&u.login))
}

fn admin_gone() -> Response {
    error_page(StatusCode::NOT_FOUND, "No such page.")
}

pub async fn admin(State(state): State<AppState>, jar: CookieJar) -> Result<Response, AppError> {
    let Some(viewer) = require_admin(&state, &jar).await? else {
        return Ok(admin_gone());
    };

    let now = state.now();
    let round = Round::current(now);
    let round_key = round.key();
    let offset = state.clock_offset();

    let last_round = db::round_summary(
        &state.db,
        Pool::Vienna,
        &round_key,
        &round.previous_date().format("%Y-%m-%d").to_string(),
    )
    .await?
    // An admin is signed in through intra by definition.
    .map(|r| RoundView::new(r, true));

    Ok(templates::render(&AdminTemplate {
        pool: Pool::Vienna,
        user: Some(UserView::from(viewer)),
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
        pools: vec![
            templates::AdminPoolView {
                key: "vienna",
                label: "42 vienna",
                guesses: db::round_guesses(&state.db, Pool::Vienna, &round_key)
                    .await?
                    .into_iter()
                    .map(Into::into)
                    .collect(),
            },
            templates::AdminPoolView {
                key: "global",
                label: "global",
                guesses: db::round_guesses(&state.db, Pool::Global, &round_key)
                    .await?
                    .into_iter()
                    .map(Into::into)
                    .collect(),
            },
        ],
        demo_counts: db::demo_counts(&state.db).await?,
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
    /// "vienna" (the default) or "global".
    #[serde(default)]
    pool: Option<String>,
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
    // The global pool is keyed by account; a stand-in gets one like anyone
    // signing in, and so plays both pools.
    let pool = admin_pool(form.pool.as_deref());
    let id = match pool {
        Pool::Vienna => player.id,
        Pool::Global => db::account_for_intra(&state.db, player.id, &player.login).await?.id,
    };

    let participates = form.ghost.is_none();
    let round_key = Round::current(state.now()).key();
    let msg = if db::insert_guess(&state.db, pool, &round_key, id, value, participates).await? {
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
    pub login: String,
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

    let account = db::account_for_intra(&state.db, player.id, &player.login).await?;
    let token = auth::random_token();
    db::create_session(&state.db, &token, account.id, auth::SESSION_TTL_DAYS).await?;

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
    let restores_admin = db::viewer_for_session(&state.db, &parked)
        .await?
        .is_some_and(|v| is_admin(&state, &v));

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

#[derive(Deserialize)]
pub struct DemoForm {
    days: i64,
    #[serde(default)]
    pool: Option<String>,
}

/// Which pool an admin form is about: "global", or the 42 pool by default.
fn admin_pool(value: Option<&str>) -> Pool {
    match value {
        Some("global") => Pool::Global,
        _ => Pool::Vienna,
    }
}

#[derive(Deserialize)]
pub struct PoolForm {
    #[serde(default)]
    pool: Option<String>,
}

/// Fills the closed rounds before today with random bot guesses, so the
/// history pages have months of data to chew on. Test instances only.
pub async fn admin_demo(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<DemoForm>,
) -> Result<Response, AppError> {
    if require_admin(&state, &jar).await?.is_none() {
        return Ok(admin_gone());
    }
    let days = form.days.clamp(1, 730);
    let last_closed = Round::current(state.now()).previous_date();
    // Generated before any await: the thread-local RNG cannot be held across one.
    let guesses = crate::demo::generate(&mut rand::thread_rng(), last_closed, days);
    let started = std::time::Instant::now();
    let added = db::insert_demo(&state.db, admin_pool(form.pool.as_deref()), &guesses).await?;
    tracing::info!(days, added, took = ?started.elapsed(), "generated demo history");
    Ok(Redirect::to("/admin").into_response())
}

pub async fn admin_demo_remove(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response, AppError> {
    if require_admin(&state, &jar).await?.is_none() {
        return Ok(admin_gone());
    }
    db::remove_demo(&state.db).await?;
    Ok(Redirect::to("/admin").into_response())
}

pub async fn admin_clear(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<PoolForm>,
) -> Result<Response, AppError> {
    if require_admin(&state, &jar).await?.is_none() {
        return Ok(admin_gone());
    }
    let round_key = Round::current(state.now()).key();
    db::clear_round(&state.db, admin_pool(form.pool.as_deref()), &round_key).await?;
    Ok(Redirect::to("/admin?msg=cleared").into_response())
}

// -------------------------------------------------------------------- auth
//
// Signing in lives in oauth.rs (42, GitHub, Google) and signin.rs (email).

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
    use super::{names_visible, parse_guess, round_matches};
    use crate::db::Pool;
    use crate::db::RoundSummary;

    fn round(date: &str, winner: Option<(&str, i64)>) -> RoundSummary {
        RoundSummary {
            round_date: date.to_string(),
            total: 10,
            winning_value: winner.map(|w| w.1),
            winner_login: winner.map(|w| w.0.to_string()),
            winner_name: winner.map(|w| w.0.to_string()),
        }
    }

    #[test]
    fn search_finds_rounds_by_date_login_or_number() {
        let r = round("2026-08-14", Some(("megardes", 1_337)));
        assert!(round_matches(&r, "", true));
        assert!(round_matches(&r, "2026-08", true));
        assert!(round_matches(&r, "2026-08-14", true));
        assert!(round_matches(&r, "MEGA", true));
        assert!(round_matches(&r, "1337", true));
        assert!(round_matches(&r, "1 337", true));
        assert!(!round_matches(&r, "2026-09", true));
        assert!(!round_matches(&r, "133", true));
        assert!(!round_matches(&r, "someone", true));

        let nobody = round("2026-08-15", None);
        assert!(!round_matches(&nobody, "megardes", true));
        assert!(round_matches(&nobody, "2026-08", true));
    }

    #[test]
    fn a_visitor_who_cannot_see_logins_cannot_search_them() {
        let r = round("2026-08-14", Some(("megardes", 1_337)));
        // A match is itself an answer: the row would come back with the name
        // masked, but only because the name matched.
        assert!(!round_matches(&r, "megardes", false));
        assert!(!round_matches(&r, "MEGA", false));
        // Everything impersonal still searches.
        assert!(round_matches(&r, "2026-08", false));
        assert!(round_matches(&r, "1337", false));
        assert!(round_matches(&r, "", false));
    }

    #[test]
    fn names_are_shown_to_42_students_and_masked_for_everyone_else() {
        use crate::db::{Account, LeaderboardRow, User, Viewer};
        use crate::templates::{LeaderView, RoundView, MASK};

        let student = Viewer {
            account: Account { id: 1, username: "megardes".to_string(), intra_id: Some(7), verified: true },
            intra: Some(User {
                id: 7,
                login: "megardes".to_string(),
                display_name: "Meg Ardes".to_string(),
                image_url: None,
            }),
        };
        // Signed in, but not through 42: a stranger with an account must not
        // be able to read the 42 pool's logins.
        let stranger = Viewer {
            account: Account { id: 2, username: "stranger".to_string(), intra_id: None, verified: true },
            intra: None,
        };
        assert!(names_visible(Pool::Vienna, Some(&student)));
        assert!(!names_visible(Pool::Vienna, Some(&stranger)));
        assert!(!names_visible(Pool::Vienna, None));
        // The global pool goes by chosen usernames, shown to anyone signed in.
        assert!(names_visible(Pool::Global, Some(&student)));
        assert!(names_visible(Pool::Global, Some(&stranger)));
        assert!(!names_visible(Pool::Global, None));

        let r = round("2026-08-14", Some(("megardes", 1_337)));
        let shown = RoundView::new(r.clone(), true).winner.unwrap();
        assert_eq!(shown.login, "megardes");
        assert_eq!(shown.value_label, "1\u{202f}337");

        let hidden = RoundView::new(r, false).winner.unwrap();
        assert_eq!(hidden.login, MASK);
        assert_eq!(hidden.display_name, MASK);
        // The number is the game, not personal data: it stays.
        assert_eq!(hidden.value_label, "1\u{202f}337");

        let row = LeaderboardRow {
            login: "megardes".to_string(),
            display_name: "Meg Ardes".to_string(),
            wins: 12,
        };
        assert_eq!(LeaderView::new(row.clone(), true).login, "megardes");
        let hidden = LeaderView::new(row, false);
        assert_eq!(hidden.login, MASK);
        assert_eq!(hidden.display_name, MASK);
        assert_eq!(hidden.wins, 12);
    }

    // ------------------------------------------------ through the handlers
    //
    // These call the real handlers against a real database, signed in through
    // a real session cookie, and look at what comes back.

    mod live {
        use axum::extract::{Form, Query, State};
        use axum_extra::extract::cookie::{Cookie, CookieJar};

        use crate::app::AppState;
        use crate::auth;
        use crate::db::{self, Pool};
        use crate::handlers::{self, GuessForm, ImpersonateForm, ResultsQuery};
        use crate::round::Round;
        use crate::testkit::{app, body, jar, location, stranger, student};

        fn guess(state: &AppState, value: &str) -> Form<GuessForm> {
            Form(GuessForm {
                guess: value.to_string(),
                round: Round::current(state.now()).key(),
            })
        }

        async fn guesses(db: &db::Db) -> Vec<(i64, i64)> {
            sqlx::query_as("SELECT user_id, value FROM guesses ORDER BY user_id")
                .fetch_all(db)
                .await
                .unwrap()
        }

        #[tokio::test]
        async fn a_42_students_guess_is_stored_under_their_intra_id() {
            let (t, state) = app(&[]).await;
            // Pad the accounts table so account ids and intra ids cannot line up.
            stranger(&t.db, "padding", "p").await;
            student(&t.db, 1001, "megardes", "tok").await;

            let resp = handlers::confirm_guess(State(state.clone()), jar("tok"), guess(&state, "7"))
                .await
                .unwrap();
            assert_eq!(location(&resp), "/?msg=saved");
            assert_eq!(guesses(&t.db).await, [(1001, 7)]);
        }

        #[tokio::test]
        async fn an_account_without_42_cannot_guess_in_the_42_pool() {
            let (t, state) = app(&[]).await;
            stranger(&t.db, "stranger", "tok").await;

            let resp = handlers::submit_guess(State(state.clone()), jar("tok"), guess(&state, "7"))
                .await
                .unwrap();
            assert_eq!(location(&resp), "/?msg=not_42");
            let resp = handlers::confirm_guess(State(state.clone()), jar("tok"), guess(&state, "7"))
                .await
                .unwrap();
            assert_eq!(location(&resp), "/?msg=not_42");
            assert!(guesses(&t.db).await.is_empty(), "nothing may be written");

            // Signed out is still told to sign in, as before.
            let resp = handlers::confirm_guess(State(state.clone()), CookieJar::new(), guess(&state, "7"))
                .await
                .unwrap();
            assert_eq!(location(&resp), "/?msg=login_required");
        }

        #[tokio::test]
        async fn the_front_page_offers_the_form_only_to_42_students() {
            let (t, state) = app(&[]).await;
            student(&t.db, 1001, "megardes", "s").await;
            stranger(&t.db, "stranger", "x").await;
            let page = |token: Option<&'static str>| {
                let state = state.clone();
                async move {
                    let jar = token.map(jar).unwrap_or_default();
                    body(handlers::index(State(state), jar, Query(handlers::IndexQuery { msg: None }))
                        .await
                        .unwrap())
                    .await
                }
            };

            let html = page(Some("s")).await;
            assert!(html.contains(r#"action="/guess""#), "a student gets the form");
            assert!(html.contains(">megardes<"));

            let html = page(Some("x")).await;
            assert!(!html.contains(r#"action="/guess""#), "a stranger gets no form");
            assert!(html.contains("this round is for 42 Vienna students"));
            assert!(html.contains(">stranger<"), "but is shown as signed in");

            let html = page(None).await;
            assert!(!html.contains(r#"action="/guess""#));
            assert!(html.contains("log in to play"));
        }

        #[tokio::test]
        async fn results_hide_42_names_from_an_account_without_42() {
            let (t, state) = app(&[]).await;
            student(&t.db, 1001, "megardes", "s").await;
            stranger(&t.db, "stranger", "x").await;
            // A closed round that megardes won.
            let yesterday = Round::current(state.now()).previous_date().format("%Y-%m-%d").to_string();
            db::insert_guess(&t.db, Pool::Vienna, &yesterday, 1001, 3, true).await.unwrap();

            let page = |token: &'static str, q: &str| {
                let state = state.clone();
                let q = Some(q.to_string());
                async move {
                    body(handlers::results(
                        State(state),
                        jar(token),
                        Query(ResultsQuery { q, show: None, date: None }),
                    )
                    .await
                    .unwrap())
                    .await
                }
            };
            assert!(page("s", "").await.contains("megardes"), "a student sees who won");
            let html = page("x", "").await;
            assert!(!html.contains("megardes"), "a stranger must not");
            assert!(html.contains(crate::templates::MASK));
            // Nor find out by searching for the login.
            assert!(page("x", "megardes").await.contains("no round matches"));
            assert!(!page("s", "megardes").await.contains("no round matches"));
        }

        /// A stranger whose account is verified, as a GitHub, Google or
        /// confirmed email sign-in will be.
        async fn verified(db: &db::Db, username: &str, token: &str) -> i64 {
            stranger(db, username, token).await;
            let v = db::viewer_for_session(db, token).await.unwrap().unwrap();
            sqlx::query("UPDATE accounts SET verified_at = 'x' WHERE id = ?1")
                .bind(v.account.id)
                .execute(db)
                .await
                .unwrap();
            v.account.id
        }

        async fn global_guesses(db: &db::Db) -> Vec<(i64, i64)> {
            sqlx::query_as("SELECT user_id, value FROM global_guesses ORDER BY user_id")
                .fetch_all(db)
                .await
                .unwrap()
        }

        async fn global_page(state: &AppState, token: Option<&str>) -> String {
            let jar = token.map(jar).unwrap_or_default();
            body(
                handlers::global_index(State(state.clone()), jar, Query(handlers::IndexQuery { msg: None }))
                    .await
                    .unwrap(),
            )
            .await
        }

        #[tokio::test]
        async fn anyone_verified_plays_the_global_pool_under_their_account() {
            let (t, state) = app(&[]).await;
            let ann = verified(&t.db, "ann", "a").await;

            let html = global_page(&state, Some("a")).await;
            assert!(html.contains(r#"action="/g/guess""#), "the form posts to the global pool");

            let resp = handlers::global_submit_guess(State(state.clone()), jar("a"), guess(&state, "7"))
                .await
                .unwrap();
            assert!(body(resp).await.contains(r#"action="/g/guess/confirm""#));
            let resp = handlers::global_confirm_guess(State(state.clone()), jar("a"), guess(&state, "7"))
                .await
                .unwrap();
            assert_eq!(location(&resp), "/g?msg=saved");
            assert_eq!(global_guesses(&t.db).await, [(ann, 7)]);
            assert!(guesses(&t.db).await.is_empty(), "the 42 pool is untouched");

            let resp = handlers::global_confirm_guess(State(state.clone()), jar("a"), guess(&state, "3"))
                .await
                .unwrap();
            assert_eq!(location(&resp), "/g?msg=already");
        }

        #[tokio::test]
        async fn an_unverified_account_sees_the_global_pool_but_cannot_play() {
            let (t, state) = app(&[]).await;
            stranger(&t.db, "new", "n").await;

            let html = global_page(&state, Some("n")).await;
            assert!(!html.contains(r#"action="/g/guess""#));
            assert!(html.contains("verify your email"));
            for resp in [
                handlers::global_submit_guess(State(state.clone()), jar("n"), guess(&state, "7")).await.unwrap(),
                handlers::global_confirm_guess(State(state.clone()), jar("n"), guess(&state, "7")).await.unwrap(),
            ] {
                assert_eq!(location(&resp), "/g?msg=unverified");
            }
            assert!(global_guesses(&t.db).await.is_empty());

            let resp = handlers::global_confirm_guess(State(state.clone()), CookieJar::new(), guess(&state, "7"))
                .await
                .unwrap();
            assert_eq!(location(&resp), "/g?msg=signin");
        }

        #[tokio::test]
        async fn a_42_student_plays_both_pools_separately() {
            let (t, state) = app(&[]).await;
            student(&t.db, 1001, "megardes", "s").await;
            let account = db::viewer_for_session(&t.db, "s").await.unwrap().unwrap().account.id;

            handlers::confirm_guess(State(state.clone()), jar("s"), guess(&state, "4")).await.unwrap();
            handlers::global_confirm_guess(State(state.clone()), jar("s"), guess(&state, "9")).await.unwrap();
            assert_eq!(guesses(&t.db).await, [(1001, 4)]);
            assert_eq!(global_guesses(&t.db).await, [(account, 9)]);

            // Each front page shows that pool's sealed number.
            let vienna = body(
                handlers::index(State(state.clone()), jar("s"), Query(handlers::IndexQuery { msg: None }))
                    .await
                    .unwrap(),
            )
            .await;
            assert!(vienna.contains(r#"<p class="sealed-value">4</p>"#));
            assert!(global_page(&state, Some("s")).await.contains(r#"<p class="sealed-value">9</p>"#));
        }

        #[tokio::test]
        async fn global_results_name_players_to_anyone_signed_in() {
            let (t, state) = app(&[]).await;
            let ann = verified(&t.db, "ann", "a").await;
            stranger(&t.db, "watcher", "w").await;
            let yesterday = Round::current(state.now()).previous_date().format("%Y-%m-%d").to_string();
            db::insert_guess(&t.db, Pool::Global, &yesterday, ann, 3, true).await.unwrap();

            let page = |token: Option<&'static str>| {
                let state = state.clone();
                async move {
                    let jar = token.map(jar).unwrap_or_default();
                    body(
                        handlers::global_results(
                            State(state),
                            jar,
                            Query(ResultsQuery { q: None, show: None, date: None }),
                        )
                        .await
                        .unwrap(),
                    )
                    .await
                }
            };
            let html = page(Some("w")).await;
            assert!(html.contains("ann"), "an account, even unverified, sees who won");
            assert!(html.contains(&format!(r#"href="/g/day/{yesterday}""#)), "links stay in the pool");
            assert!(!page(None).await.contains("ann"), "signed out does not");
            // The 42 pool has nothing for that day.
            let vienna = body(
                handlers::results(State(state.clone()), jar("w"), Query(ResultsQuery { q: None, show: None, date: None }))
                    .await
                    .unwrap(),
            )
            .await;
            assert!(!vienna.contains(&yesterday));
        }

        #[tokio::test]
        async fn the_pool_switch_marks_where_you_are() {
            let (_t, state) = app(&[]).await;
            let global = global_page(&state, None).await;
            assert!(global.contains(r#"<a class="on" aria-current="true" href="/g/">global</a>"#));
            assert!(global.contains(r#"href="/g/results""#));
            let vienna = body(
                handlers::index(State(state.clone()), CookieJar::new(), Query(handlers::IndexQuery { msg: None }))
                    .await
                    .unwrap(),
            )
            .await;
            assert!(vienna.contains(r#"<a class="on" aria-current="true" href="/">42 vienna</a>"#));
            assert!(vienna.contains(r#"href="/results""#));
        }

        #[tokio::test]
        async fn a_username_that_matches_an_admin_login_is_not_an_admin() {
            let (t, state) = app(&["megardes"]).await;
            // Someone without 42 who calls themselves megardes.
            stranger(&t.db, "megardes", "fake").await;
            assert!(handlers::require_admin(&state, &jar("fake")).await.unwrap().is_none());

            // The real one, signing in through 42, is.
            student(&t.db, 1001, "megardes", "real").await;
            let v = handlers::require_admin(&state, &jar("real")).await.unwrap().unwrap();
            assert_eq!(v.intra.unwrap().id, 1001);
            assert_eq!(v.account.username, "megardes-42", "the name was taken first");
        }

        #[tokio::test]
        async fn an_admin_can_become_a_stand_in_and_come_back() {
            let (t, state) = app(&["megardes"]).await;
            student(&t.db, 1001, "megardes", "mine").await;

            let resp = handlers::admin_impersonate(
                State(state.clone()),
                jar("mine"),
                Form(ImpersonateForm { login: "alice".into() }),
            )
            .await
            .unwrap();
            // The response carries the new session and parks the old one.
            let cookies: Vec<Cookie> = resp
                .headers()
                .get_all(axum::http::header::SET_COOKIE)
                .iter()
                .map(|v| Cookie::parse(v.to_str().unwrap().to_string()).unwrap())
                .collect();
            let get = |name: &str| cookies.iter().find(|c| c.name() == name).unwrap().value().to_string();
            let demo = get(auth::SESSION_COOKIE);
            assert_eq!(get(auth::ADMIN_RETURN_COOKIE), "mine");

            let v = db::viewer_for_session(&t.db, &demo).await.unwrap().unwrap();
            let alice = v.intra.unwrap();
            assert_eq!(alice.login, "alice");
            assert!(alice.id < 0, "a stand-in, never a real account");

            // A stand-in plays the 42 pool on a test instance.
            let resp = handlers::confirm_guess(State(state.clone()), jar(&demo), guess(&state, "5"))
                .await
                .unwrap();
            assert_eq!(location(&resp), "/?msg=saved");
            assert_eq!(guesses(&t.db).await, [(alice.id, 5)]);

            let back = jar(&demo).add(Cookie::new(auth::ADMIN_RETURN_COOKIE, "mine"));
            let resp = handlers::admin_return(State(state.clone()), back).await.unwrap();
            assert_eq!(location(&resp), "/admin");
            assert!(db::viewer_for_session(&t.db, &demo).await.unwrap().is_none(), "demo session dropped");
            assert!(db::viewer_for_session(&t.db, "mine").await.unwrap().is_some());
        }

        #[tokio::test]
        async fn a_real_42_account_cannot_be_impersonated() {
            let (t, state) = app(&["megardes"]).await;
            student(&t.db, 1001, "megardes", "mine").await;
            student(&t.db, 1002, "classmate", "theirs").await;

            let resp = handlers::admin_impersonate(
                State(state.clone()),
                jar("mine"),
                Form(ImpersonateForm { login: "classmate".into() }),
            )
            .await
            .unwrap();
            assert_eq!(location(&resp), "/admin?msg=not_demo");
            assert_eq!(
                sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM account_sessions")
                    .fetch_one(&t.db)
                    .await
                    .unwrap()
                    .0,
                2,
                "no new session"
            );
        }
    }

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
