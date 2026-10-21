use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use axum_extra::extract::cookie::CookieJar;
use chrono::Utc;
use serde::Deserialize;

use crate::app::{error_page, AppError, AppState, ClientIp};
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
const PODIUM: usize = 10;
/// How many earlier rounds a day page lists under itself: at most ten
/// rows before "every round" (spec R11.4).
const DAY_HISTORY: i64 = 10;

pub(crate) async fn current_viewer(state: &AppState, jar: &CookieJar) -> Result<Option<Viewer>, AppError> {
    let Some(token) = auth::session_token(jar) else {
        return Ok(None);
    };
    Ok(db::viewer_for_session(&state.db, &token).await?)
}

/// Flash messages travel as a fixed code in the query string, so nothing a
/// visitor types is ever echoed back into the page.
fn notice_for(code: Option<&str>) -> Option<Notice> {
    let (kind, text) = crate::copy::notice(code?)?;
    Some(if kind == "ok" { Notice::ok(text) } else { Notice::error(text) })
}

/// Whether this visitor may be shown who played in `pool`.
///
/// In the 42 pool a login and a real name are a 42 student's personal data,
/// so they leave the server only for a visitor who plays as a 42 student;
/// being signed in is not enough, since an account need not be a 42 one.
/// In the global pool everyone goes by a username they chose, and usernames
/// are public (spec R9.2): shown to every visitor, signed in or not.
pub(crate) fn names_visible(pool: Pool, viewer: Option<&Viewer>) -> bool {
    match pool {
        Pool::Vienna => viewer.is_some_and(|v| v.intra.is_some()),
        Pool::Global => true,
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

/// The 42 round is only there for 42 students. Anyone else who asks for one
/// of its pages - signed out, or signed in some other way - is sent to the
/// same page of the global round, and is never told the 42 round exists.
/// `None` when the visitor may stay.
pub(crate) fn outside_42(pool: Pool, viewer: Option<&Viewer>, global_page: &str) -> Option<Response> {
    let sees_42 = viewer.is_some_and(|v| v.intra.is_some());
    (pool == Pool::Vienna && !sees_42).then(|| Redirect::to(global_page).into_response())
}

/// Where a pool's front page lives, for redirects back to it.
fn home(pool: Pool) -> &'static str {
    match pool {
        Pool::Vienna => "/",
        Pool::Global => "/g",
    }
}



#[derive(Deserialize)]
pub struct IndexQuery {
    pub msg: Option<String>,
    /// The number a "held_late" notice is about: digits only, so safe to show.
    #[serde(default)]
    pub n: Option<i64>,
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

/// `POST /g/streak`: credits every streak bonus waiting, then back to the
/// round. Safe to send twice: each bonus is a ledger row that exists once.
pub async fn claim_streak(State(state): State<AppState>, jar: CookieJar) -> Result<Response, AppError> {
    let Some(viewer) = current_viewer(&state, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    if !viewer.account.verified {
        return Ok(Redirect::to("/g/?msg=unverified").into_response());
    }
    let got = crate::economy::claim_streaks(&state.db, viewer.account.id).await?;
    Ok(Redirect::to(if got > 0 { "/g/?msg=streak_claimed" } else { "/g/" }).into_response())
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

    let mut viewer = current_viewer(&state, &jar).await?;
    if let Some(away) = outside_42(pool, viewer.as_ref(), "/g/") {
        return Ok(away);
    }
    let player = viewer.as_ref().and_then(|v| player_in(pool, v));
    // The global round: pay out anything that closed since the settler last
    // looked, so the page never shows an unpaid pot, and hand a player who
    // turned up their points for today.
    let pot = if pool == Pool::Global {
        crate::economy::settle_all(&state.db, &round_key).await?;
        if let Some(p) = player {
            crate::economy::collect(&state.db, p, &round_key).await?;
            viewer = current_viewer(&state, &jar).await?;
        }
        Some(db::pot_so_far(&state.db, &round_key).await?)
    } else {
        None
    };
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
    let guess_count = db::guess_count(&state.db, pool, &round_key).await?;
    let stats = match player {
        Some(p) => Some(player_record(&state, pool, p, &round_key).await?.0),
        None => None,
    };
    // Ten days in a row in the global round is worth a bonus, claimed from a
    // pop-up here: the total waiting, if any.
    let streak_bonus = match player.filter(|_| pool == Pool::Global) {
        Some(p) => {
            let waiting = crate::economy::unclaimed_streaks(&state.db, p).await?.len() as i64;
            (waiting > 0).then(|| crate::templates::points_label(&(waiting * crate::economy::STREAK_BONUS)))
        }
        None => None,
    };
    // "saved" is said by the confirmation panel itself, not by a notice.
    let just_locked = query.msg.as_deref() == Some("saved") && my_guess_label.is_some();
    let notice = match query.msg.as_deref() {
        // Both name the round that is open now, which a fixed text cannot.
        Some("rolled") => Some(Notice::error(crate::copy::too_late())),
        Some("held_late") => Some(Notice::error(crate::copy::closed_during_signin(
            query.n.map(group_digits).as_deref(),
            &round.deadline.with_timezone(&crate::round::TZ).format("%H:%M").to_string(),
        ))),
        Some("held_existing") => my_guess_label
            .as_deref()
            .map(|n| Notice::error(crate::copy::already_locked(n))),
        code => notice_for(code).filter(|_| !just_locked),
    };
    Ok(templates::render(&IndexTemplate {
        pool,
        streak_bonus,
        can_play: player.is_some(),
        signed_in: viewer.is_some(),
        user: viewer.map(UserView::from),
        test_mode: state.cfg.test_env(),
        impersonating: auth::admin_return_token(&jar).is_some(),
        calendar_href: format!("{}/round/{round_key}.ics", pool.base()),
        stats,
        pot_label: pot.map(|(stakes, carried)| crate::templates::points_label(&(stakes + carried))),
        carried_label: pot
            .filter(|(_, carried)| *carried > 0)
            .map(|(_, carried)| crate::templates::points_label(&carried)),
        stake_label: (pool == Pool::Global).then(|| crate::templates::points_label(&crate::economy::GUESS_STAKE)),
        bet_teaser: match pool {
            Pool::Global => Some(crate::bets::teaser(&state).await?),
            Pool::Vienna => None,
        },
        round_key,
        time_left: crate::round::clock(seconds_left),
        time_left_spoken: crate::round::spoken_duration(seconds_left),
        closes_at: round.deadline.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        // The game's clock (an admin may have moved it), to the millisecond:
        // the countdown runs on the server's idea of now, not the visitor's.
        server_now: now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        close_time: round.deadline.with_timezone(&crate::round::TZ).format("%H:%M").to_string(),
        progress_pct: stats::round_progress(seconds_left),
        my_guess_label,
        just_locked,
        guess_count,
        last_round,
        announce_left,
        points_announced,
        notice,
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
    if r.round_date.starts_with(&q) || r.round_date.starts_with(&templates::iso_prefix(&q)) {
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
    let viewer = current_viewer(&state, &jar).await?;
    if let Some(away) = outside_42(pool, viewer.as_ref(), "/g/results") {
        return Ok(away);
    }
    if let Some(date) = query.date.as_deref().filter(|d| !d.is_empty()) {
        // Typed the European way (21.10.2026), or ISO from an old link. Only
        // a well-formed date is echoed into a URL, and always as ISO.
        let parsed = chrono::NaiveDate::parse_from_str(date.trim(), "%d.%m.%Y")
            .or_else(|_| chrono::NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d"));
        if let Ok(day) = parsed {
            return Ok(Redirect::to(&format!("{}/day/{}", pool.base(), day.format("%Y-%m-%d"))).into_response());
        }
    }

    let round_key = Round::current(state.now()).key();

    let named = names_visible(pool, viewer.as_ref());

    let all = db::closed_rounds(&state.db, pool, &round_key, HISTORY_LIMIT).await?;
    let total_rounds = all.len();

    // The round that closed most recently, said in one line: it is what
    // most people open /results for. Its chart is on its day page.
    let latest = all.first().cloned().map(|newest| RoundView::new(newest, named));
    // The last round's chart, as on its day page, with the visitor's own
    // pick marked if they played.
    let mut analysis = None;
    let chart = match all.first() {
        Some(newest) => {
            let tallies = db::round_tallies(&state.db, pool, &newest.round_date).await?;
            let day = stats::analyse(&tallies);
            // The day in two lines, as its day page has them: why the winner
            // won, and the spread of picks in numbers.
            let collided = stats::collided_below(&tallies, day.winner, 6);
            analysis = Some((why_line(day.winner, &collided), chart_summary(&tallies, day.winner, newest.total)));
            let mut chart = stats::day_chart(&tallies, &day);
            let player = viewer.as_ref().and_then(|v| player_in(pool, v));
            if let Some(p) = player {
                if let Some(mine) = db::my_guess(&state.db, pool, &newest.round_date, p).await? {
                    chart.mark_mine(mine);
                }
            }
            Some(chart)
        }
        None => None,
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

    let board = db::leaderboard(&state.db, pool, &round_key, LEADERBOARD_LIMIT).await?;
    let places = templates::ranks(&board.iter().map(|r| r.wins).collect::<Vec<_>>());
    let mut leaders: Vec<templates::LeaderView> = board
        .into_iter()
        .zip(places)
        .map(|(r, rank)| templates::LeaderView { rank, ..templates::LeaderView::new(r, named) })
        .collect();
    let rest = leaders.split_off(leaders.len().min(PODIUM));

    Ok(templates::render(&ResultsTemplate {
        pool,
        user: viewer.map(UserView::from),
        test_mode: state.cfg.test_env(),
        impersonating: auth::admin_return_token(&jar).is_some(),
        latest,
        chart,
        why: analysis.as_ref().and_then(|a| a.0.clone()),
        analysis: analysis.map(|a| a.1),
        data_source: data_source(&state, pool).await?,
        first_close: Round::current(state.now())
            .deadline
            .with_timezone(&crate::round::TZ)
            .format("%H:%M")
            .to_string(),
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
    let no_such_day = || error_page(StatusCode::NOT_FOUND, crate::copy::NO_DAY);

    // Only well-formed dates reach the database.
    let is_date = chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d")
        .is_ok_and(|d| d.format("%Y-%m-%d").to_string() == date);
    let viewer = current_viewer(&state, &jar).await?;
    let global_day = if is_date { format!("/g/day/{date}") } else { "/g/".to_string() };
    if let Some(away) = outside_42(pool, viewer.as_ref(), &global_day) {
        return Ok(away);
    }
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
    let mut chart = stats::day_chart(&tallies, &stats);
    let named = names_visible(pool, viewer.as_ref());

    // The visitor's own pick that day, marked on the chart, and the text to
    // share it with - which says how it went, never what it was.
    let player = viewer.as_ref().and_then(|v| player_in(pool, v));
    let mine = match player {
        Some(p) => db::my_guess(&state.db, pool, &date, p).await?,
        None => None,
    };
    let share_text = match (mine, player) {
        (Some(value), Some(p)) => {
            chart.mark_mine(value);
            let count = tallies.iter().find(|t| t.value == value).map_or(1, |t| t.count);
            let record = player_record(&state, pool, p, &open_round).await?;
            Some(templates::share_text(
                &date,
                &templates::outcome_symbol(value, count, stats.winner),
                stats.winner,
                summary.total,
                record.0.streak,
                &format!("{}{}/day/{date}", state.cfg.public_url, pool.base()),
            ))
        }
        _ => None,
    };
    let collided = stats::collided_below(&tallies, stats.winner, 6);
    let takeaway = takeaway_line(stats.winner, summary.total, &collided);
    let summary_text = chart_summary(&tallies, stats.winner, summary.total);
    let rows = tallies
        .iter()
        .map(|t| templates::PickRow {
            value_label: group_digits(t.value),
            count: t.count,
            fate: if Some(t.value) == stats.winner {
                "won"
            } else if t.count > 1 {
                "taken"
            } else {
                "unique"
            },
        })
        .collect();

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
    let previous = earlier.first().map(|r| r.date_key.clone());
    let data_source = data_source(&state, pool).await?;
    let pot_line = match pool {
        // Said only when there was a pot: a round played before points
        // existed, or by bots, has nothing to say here.
        Pool::Global => db::pot(&state.db, &date).await?.and_then(|p| {
            let total = templates::points_label(&(p.stakes + p.carried_in));
            if p.voided {
                Some("called off · stakes refunded".to_string())
            } else if p.stakes + p.carried_in == 0 {
                None
            } else if p.winner_id.is_some() {
                Some(format!("pot {total} → winner"))
            } else {
                Some(format!("no winner · pot {total} rolled over"))
            }
        }),
        Pool::Vienna => None,
    };

    Ok(templates::render(&DayTemplate {
        pool,
        previous,
        pot_line,
        takeaway,
        summary: summary_text,
        data_source,
        my_pick: mine.map(group_digits),
        share_text,
        rows,
        user: viewer.map(UserView::from),
        test_mode: state.cfg.test_env(),
        impersonating: auth::admin_return_token(&jar).is_some(),
        round: RoundView::new(summary, named),
        distinct: stats.distinct,
        lowest_unpicked_label: group_digits(stats.lowest_unpicked),
        most: stats.most.iter().take(DAY_LIST).map(Into::into).collect(),
        least: {
            // Fewest picks first; ties to the lower number.
            let mut by_fewest = tallies.clone();
            by_fewest.sort_by_key(|t| (t.count, t.value));
            by_fewest.iter().take(DAY_LIST).map(Into::into).collect()
        },
        chart,
        earlier,
        newer,
        total_rounds,
    }))
}

/// How many numbers the day page lists as most and least picked.
const DAY_LIST: usize = 5;

/// `/me` and `/g/me`: the signed-in player's record and rounds (R14.2).
pub async fn me(State(state): State<AppState>, jar: CookieJar) -> Result<Response, AppError> {
    me_in(Pool::Vienna, state, jar).await
}

pub async fn global_me(State(state): State<AppState>, jar: CookieJar) -> Result<Response, AppError> {
    me_in(Pool::Global, state, jar).await
}

async fn me_in(pool: Pool, state: AppState, jar: CookieJar) -> Result<Response, AppError> {
    let viewer = current_viewer(&state, &jar).await?;
    if let Some(away) = outside_42(pool, viewer.as_ref(), "/g/me") {
        return Ok(away);
    }
    let Some(viewer) = viewer else {
        return Ok(Redirect::to("/login").into_response());
    };
    let open_round = Round::current(state.now()).key();
    let player = player_in(pool, &viewer);
    let (stats, history) = match player {
        Some(p) => player_record(&state, pool, p, &open_round).await?,
        None => Default::default(),
    };
    let mut rounds: Vec<templates::MyRoundView> = history
        .iter()
        .map(|r| templates::MyRoundView {
            date: r.round_date.clone(),
            pick: group_digits(r.value),
            winner: r.winning_value.map(group_digits),
            won: r.won(),
        })
        .collect();
    let older = rounds.split_off(rounds.len().min(30));
    Ok(templates::render(&templates::MeTemplate {
        pool,
        test_mode: state.cfg.test_env(),
        impersonating: auth::admin_return_token(&jar).is_some(),
        best_label: stats.best.map(group_digits),
        stats,
        rounds,
        older,
        cannot_play: player.is_none(),
        user: Some(UserView::from(viewer)),
    }))
}

/// "5 won · 41 players · 1, 2, 3 and 4 were all taken" (R11.1).
fn takeaway_line(winner: Option<i64>, players: i64, collided: &[i64]) -> String {
    let who = match winner {
        Some(w) => format!("{} won", group_digits(w)),
        None => "nobody won".to_string(),
    };
    let players = format!("{players} {}", if players == 1 { "player" } else { "players" });
    match why_line(winner, collided) {
        Some(why) => format!("{who} · {players} · {why}"),
        None => format!("{who} · {players}"),
    }
}

/// Why the winner won: "1, 2 and 4 were all taken", or "nothing lower was
/// picked". None when nobody won and nothing collided below.
fn why_line(winner: Option<i64>, collided: &[i64]) -> Option<String> {
    match collided {
        [] => winner.map(|_| "nothing lower was picked".to_string()),
        [one] => Some(format!("{} was taken", group_digits(*one))),
        many => {
            let (last, rest) = many.split_last().expect("not empty");
            let rest: Vec<String> = rest.iter().map(|v| group_digits(*v)).collect();
            Some(format!("{} and {} were all taken", rest.join(", "), group_digits(*last)))
        }
    }
}

/// The day's chart in words (R11.3).
fn chart_summary(tallies: &[stats::Tally], winner: Option<i64>, players: i64) -> String {
    let distinct = tallies.len();
    let shared = tallies.iter().filter(|t| t.count > 1).count();
    let most = tallies.iter().max_by_key(|t| (t.count, -t.value));
    let most = most
        .filter(|t| t.count > 1)
        .map(|t| format!("most: {} ({}×)", group_digits(t.value), t.count))
        .unwrap_or_else(|| "no repeats".to_string());
    let winner = winner
        .map(|w| format!("winner: {}", group_digits(w)))
        .unwrap_or_else(|| "no winner".to_string());
    format!(
        "{players} {} · {distinct} {} · {shared} shared numbers · {most} · {winner}",
        if players == 1 { "player" } else { "players" },
        if distinct == 1 { "number" } else { "numbers" },
    )
}

/// Where the numbers on a page come from (R9.1). A test instance says test,
/// or seeded once demo bots have played; a live site's closed rounds are
/// historical.
async fn data_source(state: &AppState, pool: Pool) -> Result<&'static str, AppError> {
    Ok(if !state.cfg.test_env() {
        "historical"
    } else if db::has_demo_data(&state.db, pool).await? {
        "seeded"
    } else {
        "test"
    })
}

/// A player's record in `pool` and the rounds behind it (R14, D5).
async fn player_record(
    state: &AppState,
    pool: Pool,
    player: i64,
    open_round: &str,
) -> Result<(stats::PlayerStats, Vec<db::PlayedRound>), AppError> {
    let history = db::played_rounds(&state.db, pool, open_round, player).await?;
    let guessed_open = db::my_guess(&state.db, pool, open_round, player).await?.is_some();
    let dated: Vec<(chrono::NaiveDate, i64, bool)> = history
        .iter()
        .filter_map(|r| {
            let day = chrono::NaiveDate::parse_from_str(&r.round_date, "%Y-%m-%d").ok()?;
            Some((day, r.value, r.won()))
        })
        .collect();
    let last_closed = chrono::NaiveDate::parse_from_str(open_round, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.pred_opt())
        .unwrap_or_default();
    Ok((stats::player_stats(&dated, guessed_open, last_closed), history))
}

#[derive(Deserialize, Default)]
pub struct TrendsQuery {
    /// "7" (default), "30", "all" or "custom".
    pub days: Option<String>,
    /// The two spans a custom comparison sets side by side, typed the
    /// European way (21.10.2026); ISO works too.
    #[serde(default)]
    pub a_from: Option<String>,
    #[serde(default)]
    pub a_to: Option<String>,
    #[serde(default)]
    pub b_from: Option<String>,
    #[serde(default)]
    pub b_to: Option<String>,
}

/// A span of days, first and last.
type Span = (chrono::NaiveDate, chrono::NaiveDate);

/// A date typed into the trends form: 21.10.2026, or ISO.
fn typed_date(raw: Option<&str>) -> Option<chrono::NaiveDate> {
    let raw = raw?.trim();
    chrono::NaiveDate::parse_from_str(raw, "%d.%m.%Y")
        .or_else(|_| chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d"))
        .ok()
}

/// The two spans of a custom comparison: what was typed, else the first
/// week of last month against the last seven closed days. Each span runs
/// from its first date to its last, in whichever order they were typed,
/// and stops at the last closed round. `Err` names what was wrong.
fn custom_spans(
    q: &TrendsQuery,
    last_closed: chrono::NaiveDate,
) -> Result<(Span, Span), &'static str> {
    use chrono::Datelike;
    let first_of_this = last_closed.with_day(1).expect("day 1 exists");
    let first_of_last = (first_of_this - chrono::Duration::days(1)).with_day(1).expect("day 1 exists");
    let defaults = [
        first_of_last,
        first_of_last + chrono::Duration::days(6),
        last_closed - chrono::Duration::days(6),
        last_closed,
    ];
    let typed = [&q.a_from, &q.a_to, &q.b_from, &q.b_to];
    let mut dates = defaults;
    for (i, raw) in typed.iter().enumerate() {
        match raw.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
            None => {}
            Some(r) => dates[i] = typed_date(Some(r)).ok_or("dates look like 21.10.2026")?,
        }
    }
    let span = |a: chrono::NaiveDate, b: chrono::NaiveDate| (a.min(b), a.max(b).min(last_closed));
    let (a, b) = (span(dates[0], dates[1]), span(dates[2], dates[3]));
    if a.0 > last_closed || b.0 > last_closed {
        return Err("a span starts after the last closed round");
    }
    Ok((a, b))
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
    let viewer = current_viewer(&state, &jar).await?;
    if let Some(away) = outside_42(pool, viewer.as_ref(), "/g/trends") {
        return Ok(away);
    }
    let round = Round::current(state.now());
    let open_round = round.key();
    let iso = |d: chrono::NaiveDate| d.format("%Y-%m-%d").to_string();
    // Timed from the query to the finished view model, and shown on the
    // page: an honest number for how heavy the analysis is.
    let started = std::time::Instant::now();
    let took = |page: &mut TrendsTemplate| page.took = format!("{:.1} ms", started.elapsed().as_secs_f64() * 1000.0);

    // Two spans of the visitor's choosing, set side by side.
    if query.days.as_deref() == Some("custom") {
        let spans = custom_spans(&query, round.previous_date());
        let ((a_from, a_to), (b_from, b_to)) = spans.unwrap_or_else(|_| {
            let d = round.previous_date();
            ((d, d), (d, d))
        });
        let (trend, error) = match spans {
            Ok(_) => {
                let start = iso(a_from.min(b_from));
                let rows: Vec<stats::DayTally> = db::range_tallies(&state.db, pool, &start, &open_round)
                    .await?
                    .into_iter()
                    .filter(|r| {
                        let d = r.round_date.as_str();
                        (d >= iso(a_from).as_str() && d <= iso(a_to).as_str())
                            || (d >= iso(b_from).as_str() && d <= iso(b_to).as_str())
                    })
                    .collect();
                let (af, at) = (iso(a_from), iso(a_to));
                (stats::trend_split(&rows, stats::Split::First(&af, &at)), None)
            }
            Err(why) => (stats::trend(&[]), Some(why)),
        };
        let mut page = TrendsTemplate::build(
            pool,
            viewer.map(UserView::from),
            state.cfg.test_env(),
            auth::admin_return_token(&jar).is_some(),
            "custom",
            &trend,
        );
        page.span_inputs = [a_from, a_to, b_from, b_to].map(|d| templates::eu(&iso(d)));
        // What was typed, if it did not parse, so it can be corrected.
        if error.is_some() {
            page.span_inputs = [&query.a_from, &query.a_to, &query.b_from, &query.b_to]
                .map(|v| v.clone().unwrap_or_default());
        }
        page.span_error = error;
        took(&mut page);
        page.older_label = "span 1";
        page.newer_label = "span 2";
        page.first_close = round.deadline_human();
        return Ok(templates::render(&page));
    }

    let (range, days) = match query.days.as_deref() {
        Some("30") => ("30", Some(30)),
        Some("all") => ("all", None),
        _ => ("7", Some(7)),
    };
    // The range counts back from the most recent closed round.
    let from = match days {
        Some(n) => iso(round.previous_date() - chrono::Duration::days(n - 1)),
        None => String::new(), // sorts before every date
    };

    let rows = db::range_tallies(&state.db, pool, &from, &open_round).await?;
    let trend = stats::trend(&rows);
    // The form starts from the defaults, for switching to custom.
    let defaults = custom_spans(&query, round.previous_date()).ok();
    let mut page = TrendsTemplate::build(
        pool,
        viewer.map(UserView::from),
        state.cfg.test_env(),
        auth::admin_return_token(&jar).is_some(),
        range,
        &trend,
    );
    took(&mut page);
    page.first_close = round.deadline_human();
    if let Some(((a1, a2), (b1, b2))) = defaults {
        page.span_inputs = [a1, a2, b1, b2].map(|d| templates::eu(&iso(d)));
    }
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
    let viewer = current_viewer(state, jar).await?;
    if let Some(away) = outside_42(pool, viewer.as_ref(), "/g/") {
        return Ok(Err(away));
    }
    let Some(viewer) = viewer else {
        return Ok(Err(back(pool, "signin")));
    };
    match player_in(pool, &viewer) {
        Some(player) => Ok(Ok((viewer, player))),
        None => Ok(Err(back(pool, "unverified"))),
    }
}

async fn submit_guess_in(
    pool: Pool,
    state: AppState,
    jar: CookieJar,
    form: GuessForm,
) -> Result<Response, AppError> {
    let round = Round::current(state.now());
    let round_key = round.key();
    // Typed on a page whose round has since closed: say so, rather than
    // carry the number over to a round its owner never looked at.
    if !form.round.is_empty() && form.round != round_key {
        return Ok(back(pool, "rolled"));
    }

    // Signed out in the global round: hold the number, and ask them to sign
    // in to lock it. Anywhere else, signing in comes first.
    if pool == Pool::Global && current_viewer(&state, &jar).await?.is_none() {
        let value = match parse_guess(&form.guess) {
            Ok(v) => v,
            Err(code) => return Ok(back(pool, code)),
        };
        let token = state.held.hold(round_key, value);
        let jar = jar.add(crate::held::cookie(token, state.cfg.secure_cookies));
        return Ok((jar, Redirect::to("/login")).into_response());
    }

    let (viewer, player) = match player_or_back(pool, &state, &jar).await? {
        Ok(found) => found,
        Err(away) => return Ok(away),
    };

    let value = match parse_guess(&form.guess) {
        Ok(v) => v,
        Err(code) => return Ok(back(pool, code)),
    };

    if db::my_guess(&state.db, pool, &round_key, player)
        .await?
        .is_some()
    {
        return Ok(back(pool, "already"));
    }
    if pool == Pool::Global {
        crate::economy::collect(&state.db, player, &round_key).await?;
        if db::balance(&state.db, player).await? < crate::economy::GUESS_STAKE {
            return Ok(back(pool, "broke"));
        }
    }

    Ok(templates::render(&ConfirmTemplate {
        pool,
        user: Some(UserView::from(viewer)),
        test_mode: state.cfg.test_env(),
        impersonating: auth::admin_return_token(&jar).is_some(),
        value_label: group_digits(value),
        // Canonical form, so what gets stored is exactly what was shown:
        // "007" was displayed as 7 and must be submitted as 7.
        value_raw: value.to_string(),
        round_key,
        deadline_human: round.deadline_human(),
        stake_label: (pool == Pool::Global).then(|| crate::templates::points_label(&crate::economy::GUESS_STAKE)),
    }))
}

/// Step two: actually record it.
pub async fn confirm_guess(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<GuessForm>,
) -> Result<Response, AppError> {
    confirm_guess_in(Pool::Vienna, state, jar, form, None).await
}

pub async fn global_confirm_guess(
    State(state): State<AppState>,
    ip: ClientIp,
    jar: CookieJar,
    Form(form): Form<GuessForm>,
) -> Result<Response, AppError> {
    // Where a global guess came from, as a keyed hash, for spotting one
    // person playing many accounts. The 42 pool needs none of that.
    let ip_hash = crate::signin::ip_hash(&state, &ip);
    confirm_guess_in(Pool::Global, state, jar, form, ip_hash).await
}

async fn confirm_guess_in(
    pool: Pool,
    state: AppState,
    jar: CookieJar,
    form: GuessForm,
    ip_hash: Option<String>,
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

    let msg = match pool {
        // The global round: the guess and its stake, together or not at all.
        Pool::Global => {
            crate::economy::collect(&state.db, player, &round_key).await?;
            match db::lock_global_guess(&state.db, &round_key, player, value, crate::economy::GUESS_STAKE).await? {
                db::Locked::In => {
                    if let Some(hash) = ip_hash {
                        db::set_guess_ip_hash(&state.db, &round_key, player, &hash).await?;
                    }
                    "saved"
                }
                // The same guess again - a double click, a retried request:
                // it is already in, which is what was asked for.
                db::Locked::AlreadySame => "saved",
                // A different number from another tab got there first.
                db::Locked::AlreadyOther => "already",
                db::Locked::Broke => "broke",
                db::Locked::Voided => "voided",
            }
        }
        Pool::Vienna => {
            // In now, or the same number was in already: either way, saved.
            let saved = db::insert_guess(&state.db, pool, &round_key, player, value, true).await?
                || db::my_guess(&state.db, pool, &round_key, player).await? == Some(value);
            if saved { "saved" } else { "already" }
        }
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
    error_page(StatusCode::NOT_FOUND, crate::copy::NO_PAGE)
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
    pub shift: String,
}

pub async fn admin_clock(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<ClockForm>,
) -> Result<Response, AppError> {
    if require_admin(&state, &jar).await?.is_none() {
        return Ok(admin_gone());
    }

    let before = state.now();
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
    // Moved back: rounds that settled only because the clock ran ahead are
    // open again, and what they paid out goes back.
    if state.now() < before {
        let open = Round::current(state.now()).key();
        let undone = db::unsettle_from(&state.db, &open).await?;
        if undone > 0 {
            tracing::info!(from = %open, rounds = undone, "clock moved back: settlements undone");
        }
        *state.odds_base.lock().unwrap_or_else(|e| e.into_inner()) = None;
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

/// `/g/day/{date}/og.png`: the round's link-preview image (R12.2). Only for
/// closed rounds - the open one's numbers are secret - and rendered once.
pub async fn global_day_image(
    State(state): State<AppState>,
    Path(date): Path<String>,
) -> Result<Response, AppError> {
    day_image_in(Pool::Global, &state, &date).await
}

/// The 42 round's image, for 42 students only; anyone else, including the
/// bots that fetch link previews, gets the site's own picture.
pub async fn day_image(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(date): Path<String>,
) -> Result<Response, AppError> {
    let viewer = current_viewer(&state, &jar).await?;
    if let Some(away) = outside_42(Pool::Vienna, viewer.as_ref(), "/static/og.png") {
        return Ok(away);
    }
    day_image_in(Pool::Vienna, &state, &date).await
}

async fn day_image_in(pool: Pool, state: &AppState, date: &str) -> Result<Response, AppError> {
    let open_round = Round::current(state.now()).key();
    let Some(summary) = db::round_summary(&state.db, pool, &open_round, date).await? else {
        return Ok(not_found_page());
    };
    let tallies = db::round_tallies(&state.db, pool, date).await?;
    let key = format!("{}:{date}", pool.base());
    let png = state.og.get_or_render(&key, || {
        crate::og::round_image(date, &tallies, summary.winning_value, summary.total)
    });
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "image/png"),
            (axum::http::header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        png.as_ref().clone(),
    )
        .into_response())
}

/// `/round/{date}.ics` and `/g/round/{date}.ics`: the reveal of that round,
/// as a calendar event linking to its results.
pub async fn calendar(State(state): State<AppState>, Path(file): Path<String>) -> Response {
    calendar_in(Pool::Vienna, &state, &file)
}

pub async fn global_calendar(State(state): State<AppState>, Path(file): Path<String>) -> Response {
    calendar_in(Pool::Global, &state, &file)
}

fn calendar_in(pool: Pool, state: &AppState, file: &str) -> Response {
    let date = file.strip_suffix(".ics").unwrap_or("");
    let Ok(day) = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") else {
        return not_found_page();
    };
    let base = &state.cfg.public_url;
    let results = format!("{base}{}/day/{date}", pool.base());
    let host = reqwest::Url::parse(base)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "iguesslow.com".to_string());
    let key = if pool.is_global() { "global" } else { "vienna" };
    let body = crate::ics::reveal_event(date, key, crate::round::cutoff_on(day), &results, &host, Utc::now());
    let disposition = format!("attachment; filename=\"i-guess-low-{date}.ics\"");
    (
        [
            (axum::http::header::CONTENT_TYPE, "text/calendar; charset=utf-8".to_string()),
            (axum::http::header::CONTENT_DISPOSITION, disposition),
        ],
        body,
    )
        .into_response()
}

fn not_found_page() -> Response {
    error_page(StatusCode::NOT_FOUND, crate::copy::NO_PAGE)
}

/// The open round's deadline and the server's clock, for the countdown to
/// re-sync after a tab was hidden and to notice the round turning over.
pub async fn round_now(State(state): State<AppState>) -> Response {
    let now = state.now();
    let round = Round::current(now);
    // How far an admin moved the game clock, so a test instance can say so
    // instead of showing a round date that looks wrong. Always 0 when live.
    let body = format!(
        r#"{{"round":"{}","closes_at":"{}","server_now":"{}","clock_offset":{}}}"#,
        round.key(),
        round.deadline.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        state.clock_offset(),
    );
    (
        [
            (axum::http::header::CONTENT_TYPE, "application/json"),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
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
    error_page(StatusCode::NOT_FOUND, crate::copy::NO_PAGE)
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
            winner_id: winner.map(|_| 7),
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
            points: 0,
        };
        // Signed in, but not through 42: a stranger with an account must not
        // be able to read the 42 pool's logins.
        let stranger = Viewer {
            account: Account { id: 2, username: "stranger".to_string(), intra_id: None, verified: true },
            intra: None,
            points: 0,
        };
        assert!(names_visible(Pool::Vienna, Some(&student)));
        assert!(!names_visible(Pool::Vienna, Some(&stranger)));
        assert!(!names_visible(Pool::Vienna, None));
        // The global pool goes by chosen usernames, shown to anyone signed in.
        assert!(names_visible(Pool::Global, Some(&student)));
        assert!(names_visible(Pool::Global, Some(&stranger)));
        assert!(names_visible(Pool::Global, None), "usernames are public");

        let r = round("2026-08-14", Some(("megardes", 1_337)));
        let shown = RoundView::new(r.clone(), true).winner.unwrap();
        assert_eq!(shown.login, "megardes");
        assert_eq!(shown.value_label, "1\u{202f}337");

        let hidden = RoundView::new(r, false).winner.unwrap();
        assert!(hidden.login.starts_with(MASK), "{}", hidden.login);
        assert_eq!(hidden.login, crate::templates::anon_label(7), "a stable label, not a blank");
        assert_eq!(hidden.display_name, hidden.login);
        // The number is the game, not personal data: it stays.
        assert_eq!(hidden.value_label, "1\u{202f}337");

        let row = LeaderboardRow {
            id: 7,
            login: "megardes".to_string(),
            display_name: "Meg Ardes".to_string(),
            wins: 12,
        };
        assert_eq!(LeaderView::new(row.clone(), true).login, "megardes");
        let hidden = LeaderView::new(row, false);
        assert_eq!(hidden.login, crate::templates::anon_label(7));
        assert_eq!(hidden.display_name, hidden.login);
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
        async fn guessing_in_the_42_round_needs_a_42_student() {
            let (t, state) = app(&[]).await;
            stranger(&t.db, "stranger", "tok").await;

            for jar in [jar("tok"), CookieJar::new()] {
                let resp = handlers::submit_guess(State(state.clone()), jar.clone(), guess(&state, "7"))
                    .await
                    .unwrap();
                assert_eq!(location(&resp), "/g/");
                let resp = handlers::confirm_guess(State(state.clone()), jar, guess(&state, "7"))
                    .await
                    .unwrap();
                assert_eq!(location(&resp), "/g/");
            }
            assert!(guesses(&t.db).await.is_empty(), "nothing may be written");
        }

        #[tokio::test]
        async fn the_42_round_is_invisible_to_everyone_but_42_students() {
            let (t, state) = app(&[]).await;
            student(&t.db, 1001, "megardes", "s").await;
            stranger(&t.db, "stranger", "x").await;
            let yesterday = Round::current(state.now()).previous_date().format("%Y-%m-%d").to_string();
            db::insert_guess(&t.db, Pool::Vienna, &yesterday, 1001, 3, true).await.unwrap();

            // A student gets the 42 round, names and all.
            let front = handlers::index(State(state.clone()), jar("s"), Query(handlers::IndexQuery { msg: None, n: None }))
                .await
                .unwrap();
            let html = body(front).await;
            assert!(html.contains(r#"action="/guess""#));
            assert!(html.contains("megardes"));

            // Anyone else, signed in or not, is sent to the same page of the
            // global round and never sees a thing about the 42 one.
            for jar in [jar("x"), CookieJar::new()] {
                let front = handlers::index(State(state.clone()), jar.clone(), Query(handlers::IndexQuery { msg: None, n: None }))
                    .await
                    .unwrap();
                assert_eq!(location(&front), "/g/");
                let results = handlers::results(
                    State(state.clone()),
                    jar.clone(),
                    Query(ResultsQuery { q: Some("megardes".into()), show: None, date: None }),
                )
                .await
                .unwrap();
                assert_eq!(location(&results), "/g/results");
                let picked = handlers::results(
                    State(state.clone()),
                    jar.clone(),
                    Query(ResultsQuery { q: None, show: None, date: Some(yesterday.clone()) }),
                )
                .await
                .unwrap();
                assert_eq!(location(&picked), "/g/results", "not even a date jump");
                let day = handlers::day(State(state.clone()), jar.clone(), axum::extract::Path(yesterday.clone()))
                    .await
                    .unwrap();
                assert_eq!(location(&day), format!("/g/day/{yesterday}"));
                let day = handlers::day(State(state.clone()), jar.clone(), axum::extract::Path("nonsense".into()))
                    .await
                    .unwrap();
                assert_eq!(location(&day), "/g/");
                let trends = handlers::trends(State(state.clone()), jar, Query(handlers::TrendsQuery { days: None, ..Default::default() }))
                    .await
                    .unwrap();
                assert_eq!(location(&trends), "/g/trends");
            }
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
            let resp = handlers::global_confirm_guess(State(state.clone()), crate::app::ClientIp(None), jar("a"), guess(&state, "7"))
                .await
                .unwrap();
            assert_eq!(location(&resp), "/g?msg=saved");
            assert_eq!(global_guesses(&t.db).await, [(ann, 7)]);
            assert!(guesses(&t.db).await.is_empty(), "the 42 pool is untouched");

            let resp = handlers::global_confirm_guess(State(state.clone()), crate::app::ClientIp(None), jar("a"), guess(&state, "3"))
                .await
                .unwrap();
            assert_eq!(location(&resp), "/g?msg=already");
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
                handlers::global_index(State(state.clone()), jar, Query(handlers::IndexQuery { msg: None, n: None }))
                    .await
                    .unwrap(),
            )
            .await
        }

        #[tokio::test]
        async fn a_global_guess_notes_a_hash_of_where_it_came_from() {
            let mut cfg = crate::testkit::config(&[]);
            cfg.ip_hash_key = Some("k".into());
            let (t, state, _) = crate::testkit::app_with(cfg).await;
            verified(&t.db, "ann", "a").await;
            let ip = crate::app::ClientIp(Some("203.0.113.7".parse().unwrap()));
            handlers::global_confirm_guess(State(state.clone()), ip, jar("a"), guess(&state, "7")).await.unwrap();
            let (hash,): (Option<String>,) = sqlx::query_as("SELECT ip_hash FROM global_guesses").fetch_one(&t.db).await.unwrap();
            let hash = hash.unwrap();
            assert_eq!(hash.len(), 16);
            assert!(!hash.contains("203.0"));
        }

        #[tokio::test]
        async fn an_unverified_account_sees_the_global_pool_but_cannot_play() {
            let (t, state) = app(&[]).await;
            stranger(&t.db, "new", "n").await;

            let html = global_page(&state, Some("n")).await;
            assert!(!html.contains(r#"action="/g/guess""#));
            assert!(html.contains("confirm your email"));
            for resp in [
                handlers::global_submit_guess(State(state.clone()), jar("n"), guess(&state, "7")).await.unwrap(),
                handlers::global_confirm_guess(State(state.clone()), crate::app::ClientIp(None), jar("n"), guess(&state, "7")).await.unwrap(),
            ] {
                assert_eq!(location(&resp), "/g?msg=unverified");
            }
            assert!(global_guesses(&t.db).await.is_empty());

            let resp = handlers::global_confirm_guess(State(state.clone()), crate::app::ClientIp(None), CookieJar::new(), guess(&state, "7"))
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
            handlers::global_confirm_guess(State(state.clone()), crate::app::ClientIp(None), jar("s"), guess(&state, "9")).await.unwrap();
            assert_eq!(guesses(&t.db).await, [(1001, 4)]);
            assert_eq!(global_guesses(&t.db).await, [(account, 9)]);

            // Each front page shows that pool's sealed number.
            let vienna = body(
                handlers::index(State(state.clone()), jar("s"), Query(handlers::IndexQuery { msg: None, n: None }))
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
            assert!(page(None).await.contains("ann"), "usernames are public, signed out too");
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
        async fn only_42_students_are_shown_that_there_are_two_rounds() {
            let (t, state) = app(&[]).await;
            student(&t.db, 1001, "megardes", "s").await;
            verified(&t.db, "ann", "a").await;

            // The global round as anyone else sees it: no switch, no word
            // of a 42 round, and not "global" either - it is just the round.
            for token in [Some("a"), None] {
                let html = global_page(&state, token).await;
                // (The footer's "every day 12:42 vienna" is the deadline.)
                assert!(!html.contains(">42 vienna<"), "{token:?}");
                assert!(!html.contains(r#"href="/""#), "{token:?}: no link into the 42 round");
                assert!(!html.contains(r#"class="pools""#), "{token:?}");
                assert!(!html.contains("global round"), "{token:?}");
                assert!(html.contains(r#"href="/g/results""#));
            }

            // A student sees the switch in the header, on whichever side they are.
            let global = global_page(&state, Some("s")).await;
            assert!(global.contains(r#"<a class="on" aria-current="true" href="/g/" data-pool="/g">global</a>"#));
            let vienna = body(
                handlers::index(State(state.clone()), jar("s"), Query(handlers::IndexQuery { msg: None, n: None }))
                    .await
                    .unwrap(),
            )
            .await;
            assert!(vienna.contains(r#"<a class="on" aria-current="true" href="/" data-pool="">42 vienna</a>"#));
            assert!(vienna.contains(r#"href="/results""#));
            // And on every page, saying which page it is, so switching keeps you on it.
            let results = body(
                handlers::global_results(
                    State(state.clone()),
                    jar("s"),
                    Query(ResultsQuery { q: None, show: None, date: None }),
                )
                .await
                .unwrap(),
            )
            .await;
            assert!(results.contains(r#"<nav class="pools" aria-label="which round" data-here="/results">"#));
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

#[cfg(test)]
mod round_page_tests {
    //! The round page's structure (R2-R4, R6), first guesses before sign-in
    //! (R7), the confirmation and calendar (R8), and the round close (R10).

    use axum::extract::{Form, Path, Query, State};
    use axum_extra::extract::cookie::CookieJar;

    use crate::app::{AppState, ClientIp};
    use crate::db::{self, Pool};
    use crate::handlers::{self, GuessForm, IndexQuery};
    use crate::round::Round;
    use crate::testkit::{app, body, jar, location, set_cookie, stranger, student};

    async fn page(state: &AppState, jar: CookieJar, msg: Option<&str>) -> String {
        let q = Query(IndexQuery { msg: msg.map(str::to_string), n: None });
        body(handlers::global_index(State(state.clone()), jar, q).await.unwrap()).await
    }

    fn form(state: &AppState, value: &str) -> Form<GuessForm> {
        Form(GuessForm { guess: value.into(), round: Round::current(state.now()).key() })
    }

    /// A verified account with a password, as a confirmed sign-up has.
    async fn member(state: &AppState, email: &str, username: &str) -> i64 {
        let hash = crate::credentials::hash_password("correct horse battery").await.unwrap();
        let db::NewAccount::Created(a) =
            db::create_password_account(&state.db, email, username, &hash, None).await.unwrap()
        else {
            panic!("not created")
        };
        db::mark_email_verified(&state.db, a.id).await.unwrap();
        a.id
    }

    async fn sign_in(state: &AppState, jar: CookieJar, login: &str) -> axum::response::Response {
        crate::signin::login_email(
            State(state.clone()),
            ClientIp(None),
            jar,
            Form(crate::signin::EmailLoginForm { login: login.into(), password: "correct horse battery".into() }),
        )
        .await
        .unwrap()
    }

    /// Types `value` into the round while signed out; returns the cookie jar
    /// the browser would now have.
    async fn hold(state: &AppState, value: &str) -> CookieJar {
        let resp = handlers::global_submit_guess(State(state.clone()), CookieJar::new(), form(state, value))
            .await
            .unwrap();
        assert_eq!(location(&resp), "/login");
        let token = set_cookie(&resp, crate::held::COOKIE).expect("a held-guess cookie");
        crate::testkit::sent(&[(crate::held::COOKIE, &token)])
    }

    async fn guesses(db: &db::Db) -> Vec<(i64, i64)> {
        sqlx::query_as("SELECT user_id, value FROM global_guesses ORDER BY user_id, value")
            .fetch_all(db)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_round_page_reads_in_the_order_a_newcomer_needs() {
        let (_t, state) = app(&[]).await;
        let html = page(&state, CookieJar::new(), None).await;
        let at = |needle: &str| html.find(needle).unwrap_or_else(|| panic!("missing {needle:?}"));
        // The clock, the number box, the premise, the rules, then the rest.
        let order = [
            at(r#"class="countdown""#),
            at(r#"id="guess""#),
            at(r#"<p class="premise">lowest unique number wins"#),
            at("<b>01</b>"),
            at("match anyone &rarr; you both lose it."),
        ];
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");

        assert!(html.contains(r#"<span class="tag live state">open</span>"#));
        assert!(!html.contains("closes 12:42 vienna"), "no close time in the figures");
        assert!(html.contains("<b>not yet</b><span>sign in to play</span>"));
        assert!(!html.contains(">guest<"));
        // Players, the pot, you; the 42 round has no pot, so two.
        assert!(html.contains("<span>in the pot</span>"));
        crate::testkit::student(&state.db, 1001, "megardes", "s").await;
        let vienna = body(handlers::index(State(state.clone()), crate::testkit::jar("s"), Query(IndexQuery { msg: None, n: None })).await.unwrap()).await;
        assert!(vienna.contains("players this round</span></p>") && vienna.contains("<p class=\"you\">"));
        assert!(!vienna.contains("in the pot"), "two figures in the 42 round");
        assert!(html.contains("[------------------------------]") || html.contains("[#"), "the text bar");
        // Server time for the countdown, and no live region on it.
        assert!(html.contains("data-closes-at=\""));
        assert!(html.contains("data-server-now=\""));
        assert!(!html.contains("aria-live"));
        assert!(!html.contains(r#"role="timer""#));
        assert!(html.contains(r#"aria-label="closes in "#));
        // A real progress bar.
        assert!(html.contains(r#"role="progressbar""#));
        for attr in ["aria-valuemin=\"0\"", "aria-valuemax=\"100\"", "aria-valuenow=\""] {
            assert!(html.contains(attr), "{attr}");
        }
        // One primary action, and the header's sign-in is a plain link.
        assert_eq!(html.matches("class=\"primary\"").count(), 1);
        assert!(html.contains(r#"<a class="signin" href="/login">sign in</a>"#));
        assert!(!html.contains("crowded"), "no strategy hint (R22.3)");
        assert!(html.contains(r#"class="guess-field""#), "the cursor's wrapper (R23)");
    }

    #[tokio::test]
    async fn the_page_says_what_a_link_preview_needs() {
        let (_t, state) = app(&[]).await;
        let html = page(&state, CookieJar::new(), None).await;
        for tag in [
            r#"<meta property="og:title" content="i guess low">"#,
            r#"<meta property="og:type" content="website">"#,
            r#"<meta property="og:image" content="http://localhost:3000/static/og.png">"#,
            r#"<meta property="og:url" content="http://localhost:3000/g/">"#,
            r#"<meta name="twitter:card" content="summary_large_image">"#,
            r#"<link rel="apple-touch-icon" href="/static/apple-touch-icon.png">"#,
        ] {
            assert!(html.contains(tag), "{tag}");
        }
        assert!(html.contains(r#"property="og:description""#));
    }

    #[tokio::test]
    async fn a_number_typed_before_signing_in_is_locked_on_sign_in() {
        let (t, state) = app(&[]).await;
        let ann = member(&state, "ann@example.com", "ann").await;
        let held = hold(&state, "7").await;

        // The sign-in page says what is waiting.
        let login = crate::signin::login_page_handler(
            State(state.clone()),
            held.clone(),
            Query(crate::signin::LoginQuery::default()),
        )
        .await
        .unwrap();
        assert!(body(login).await.contains("sign in to lock 7"));
        assert!(guesses(&t.db).await.is_empty(), "nothing locked before signing in");

        let resp = sign_in(&state, held.clone(), "ann").await;
        assert_eq!(location(&resp), "/g?msg=saved");
        assert_eq!(guesses(&t.db).await, [(ann, 7)]);
        // The cookie goes, and the number cannot be locked a second time.
        let cleared: Vec<String> = resp.headers().get_all("set-cookie").iter()
            .map(|v| v.to_str().unwrap().to_string())
            .filter(|v| v.starts_with("ig42_held="))
            .collect();
        assert!(cleared.iter().any(|v| v.contains("Max-Age=0")), "{cleared:?}");
        sqlx::query("DELETE FROM global_guesses").execute(&t.db).await.unwrap();
        sign_in(&state, held, "ann").await;
        assert!(guesses(&t.db).await.is_empty(), "held numbers are used once");
    }

    #[tokio::test]
    async fn a_held_number_is_dropped_if_the_round_closes_meanwhile() {
        let (t, state) = app(&[]).await;
        member(&state, "ann@example.com", "ann").await;
        let held = hold(&state, "7").await;
        let now = state.now();
        state.shift_clock(Round::current(now).seconds_left(now) + 1);

        let resp = sign_in(&state, held, "ann").await;
        assert_eq!(location(&resp), "/g?msg=held_late&n=7");
        assert!(guesses(&t.db).await.is_empty());
        let resp = handlers::global_index(
            State(state.clone()),
            CookieJar::new(),
            Query(IndexQuery { msg: Some("held_late".into()), n: Some(7) }),
        )
        .await
        .unwrap();
        let html = body(resp).await;
        assert!(html.contains("round closed while you signed in · 7 not entered · next round 12:42"), "{html}");
    }

    #[tokio::test]
    async fn a_held_number_never_replaces_a_guess_already_made() {
        let (t, state) = app(&[]).await;
        let ann = member(&state, "ann@example.com", "ann").await;
        let round = Round::current(state.now()).key();
        db::insert_guess(&t.db, Pool::Global, &round, ann, 3, true).await.unwrap();
        let resp = sign_in(&state, hold(&state, "7").await, "ann").await;
        assert_eq!(location(&resp), "/g?msg=held_existing");
        assert_eq!(guesses(&t.db).await, [(ann, 3)]);
    }

    #[tokio::test]
    async fn an_unconfirmed_account_does_not_get_its_held_number_locked() {
        let (t, state) = app(&[]).await;
        let hash = crate::credentials::hash_password("correct horse battery").await.unwrap();
        db::create_password_account(&t.db, "new@example.com", "newbie", &hash, None).await.unwrap();
        let resp = sign_in(&state, hold(&state, "7").await, "newbie").await;
        assert_eq!(location(&resp), "/g?msg=held_unverified");
        assert!(guesses(&t.db).await.is_empty());
    }

    #[tokio::test]
    async fn a_bad_number_is_refused_before_anything_is_held() {
        let (_t, state) = app(&[]).await;
        let resp = handlers::global_submit_guess(State(state.clone()), CookieJar::new(), form(&state, "-3"))
            .await
            .unwrap();
        assert_eq!(location(&resp), "/g?msg=invalid");
        assert_eq!(set_cookie(&resp, crate::held::COOKIE), None);
    }

    #[tokio::test]
    async fn locking_twice_is_fine_and_a_different_number_is_not() {
        let (t, state) = app(&[]).await;
        let ann = member(&state, "ann@example.com", "ann").await;
        let session = set_cookie(&sign_in(&state, CookieJar::new(), "ann").await, crate::auth::SESSION_COOKIE).unwrap();

        for _ in 0..2 {
            let resp = handlers::global_confirm_guess(State(state.clone()), ClientIp(None), jar(&session), form(&state, "7"))
                .await
                .unwrap();
            assert_eq!(location(&resp), "/g?msg=saved", "the same guess again is a success");
        }
        let resp = handlers::global_confirm_guess(State(state.clone()), ClientIp(None), jar(&session), form(&state, "8"))
            .await
            .unwrap();
        assert_eq!(location(&resp), "/g?msg=already");
        assert_eq!(guesses(&t.db).await, [(ann, 7)]);
    }

    #[tokio::test]
    async fn the_confirmation_shows_the_number_close_time_and_calendar() {
        let (t, state) = app(&[]).await;
        let ann = member(&state, "ann@example.com", "ann").await;
        let session = set_cookie(&sign_in(&state, CookieJar::new(), "ann").await, crate::auth::SESSION_COOKIE).unwrap();
        let round = Round::current(state.now()).key();
        db::insert_guess(&t.db, Pool::Global, &round, ann, 7, true).await.unwrap();

        let html = page(&state, jar(&session), Some("saved")).await;
        assert!(html.contains("you're in"));
        assert!(html.contains(r#"<div class="sealed fresh">"#), "stamped on just now");
        assert!(html.contains(r#"<span class="stamp">sealed</span>"#));
        assert!(html.contains("reveal 12:42 &middot; <a href="), "{html}");
        assert!(!html.contains("countdown-mirror"), "one clock: the one at the top");
        assert!(html.contains(&format!(r#"<a href="/g/round/{round}.ics" download>add to calendar</a>"#)));
        assert!(!html.contains(r#"id="guess""#), "no input once locked");

        // Back later: the same box, from what the server holds, no slam.
        let html = page(&state, jar(&session), None).await;
        assert!(html.contains(r#"<div class="sealed">"#));
        assert!(html.contains(r#"<p class="sealed-value">7</p>"#));
    }

    #[tokio::test]
    async fn the_calendar_file_is_for_the_reveal_of_that_round() {
        let (_t, state) = app(&[]).await;
        let resp = handlers::global_calendar(State(state.clone()), Path("2026-10-21.ics".into())).await;
        assert_eq!(resp.headers()["content-type"], "text/calendar; charset=utf-8");
        assert!(resp.headers()["content-disposition"].to_str().unwrap().contains("i-guess-low-2026-10-21.ics"));
        let ics = body(resp).await;
        assert!(ics.contains("DTSTART:20261021T104200Z"), "12:42 CEST is 10:42 UTC");
        assert!(ics.replace("\r\n ", "").contains("URL:http://test/g/day/2026-10-21"));
        // Winter time moves it an hour in UTC.
        let ics = body(handlers::global_calendar(State(state.clone()), Path("2026-12-01.ics".into())).await).await;
        assert!(ics.contains("DTSTART:20261201T114200Z"));
        let bad = handlers::global_calendar(State(state.clone()), Path("tomorrow.ics".into())).await;
        assert_eq!(bad.status(), axum::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_guess_after_the_close_is_refused_out_loud() {
        let (t, state) = app(&[]).await;
        member(&state, "ann@example.com", "ann").await;
        let session = set_cookie(&sign_in(&state, CookieJar::new(), "ann").await, crate::auth::SESSION_COOKIE).unwrap();
        let now = state.now();
        // One second before the close: in.
        state.shift_clock(Round::current(now).seconds_left(now) - 1);
        let typed = form(&state, "7");
        let resp = handlers::global_confirm_guess(State(state.clone()), ClientIp(None), jar(&session), typed)
            .await
            .unwrap();
        assert_eq!(location(&resp), "/g?msg=saved");

        // Another player's confirmation, sent from the same page, lands one
        // second after: refused, and told so.
        stranger(&t.db, "bob", "b").await;
        sqlx::query("UPDATE accounts SET verified_at = 'x' WHERE username = 'bob'").execute(&t.db).await.unwrap();
        let late = form(&state, "8");
        state.shift_clock(2);
        let resp = handlers::global_confirm_guess(State(state.clone()), ClientIp(None), jar("b"), late)
            .await
            .unwrap();
        assert_eq!(location(&resp), "/g?msg=rolled");
        assert_eq!(guesses(&t.db).await.len(), 1);
        let html = page(&state, jar("b"), Some("rolled")).await;
        assert!(html.contains("too late · round closed"));
        // So is a first step typed on the old page.
        let resp = handlers::global_submit_guess(
            State(state.clone()),
            jar("b"),
            Form(GuessForm { guess: "8".into(), round: "2000-01-01".into() }),
        )
        .await
        .unwrap();
        assert_eq!(location(&resp), "/g?msg=rolled");
    }

    #[tokio::test]
    async fn fifty_players_picking_the_same_number_at_once_are_all_kept() {
        let (t, state) = app(&[]).await;
        let mut sessions = vec![];
        for i in 0..50 {
            stranger(&t.db, &format!("p{i}"), &format!("s{i}")).await;
            sessions.push(format!("s{i}"));
        }
        sqlx::query("UPDATE accounts SET verified_at = 'x'").execute(&t.db).await.unwrap();
        let round = Round::current(state.now()).key();
        let tasks: Vec<_> = sessions
            .into_iter()
            .map(|s| {
                let state = state.clone();
                let round = round.clone();
                tokio::spawn(async move {
                    let f = Form(GuessForm { guess: "1".into(), round });
                    handlers::global_confirm_guess(State(state), ClientIp(None), jar(&s), f).await.unwrap()
                })
            })
            .collect();
        for task in tasks {
            assert_eq!(location(&task.await.unwrap()), "/g?msg=saved");
        }
        assert_eq!(db::guess_count(&t.db, Pool::Global, &round).await.unwrap(), 50);
        let tallies = db::round_tallies(&t.db, Pool::Global, &round).await.unwrap();
        assert_eq!(tallies.len(), 1);
        assert_eq!(tallies[0].count, 50);
    }

    #[tokio::test]
    async fn the_countdown_can_ask_the_server_for_the_time() {
        let (_t, state) = app(&[]).await;
        let resp = handlers::round_now(State(state.clone())).await;
        assert_eq!(resp.headers()["cache-control"], "no-store");
        let json: serde_json::Value = serde_json::from_str(&body(resp).await).unwrap();
        assert_eq!(json["round"], Round::current(state.now()).key());
        assert!(chrono::DateTime::parse_from_rfc3339(json["closes_at"].as_str().unwrap()).is_ok());
        assert!(chrono::DateTime::parse_from_rfc3339(json["server_now"].as_str().unwrap()).is_ok());
    }

    #[tokio::test]
    async fn a_42_students_round_page_keeps_its_own_round() {
        let (_t, state) = app(&[]).await;
        student(&state.db, 1001, "megardes", "s").await;
        let html = body(
            handlers::index(State(state.clone()), jar("s"), Query(IndexQuery { msg: None, n: None })).await.unwrap(),
        )
        .await;
        assert!(html.contains(r#"action="/guess""#));
        assert!(html.contains(r#"action="/guess""#));
    }
}
