//! The pages about points in the global round: your own history at
//! `/g/points`, and the leaderboards at `/g/leaderboard` (points, then
//! wins) and `/leaderboard` (the 42 round: wins).

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;

use crate::app::{AppError, AppState};
use crate::db::{self, Pool};
use crate::handlers::current_viewer;
use crate::round::Round;
use crate::templates::{self, points_label, LedgerView, PointsBoardRow, PointsTemplate, BoardTemplate, UserView};

/// How many lines of history the points page shows.
const HISTORY: i64 = 200;
const BOARD: i64 = 100;

/// What a ledger line was, in words: to its owner, or to a moderator
/// looking at someone else's. A bet's lines say what it was on.
fn what(r: &db::LedgerRow, own: bool) -> String {
    let note = r.note.as_deref();
    let on = r
        .market
        .as_deref()
        .and_then(crate::odds::Market::parse)
        .map(|m| match r.odds_x100 {
            Some(o) => format!(": {} at {}", m.label(), crate::odds::odds_label(o)),
            None => format!(": {}", m.label()),
        })
        .unwrap_or_default();
    match r.kind.as_str() {
        "signup" => "welcome points".to_string(),
        "grant" => "daily points".to_string(),
        "stake" if own => "your guess".to_string(),
        "stake" => "guess".to_string(),
        "pot" => "won the pot".to_string(),
        "refund" => format!("refund{}{on}", note.map(|n| format!(" ({n})")).unwrap_or_default()),
        "adjust" => format!("adjusted by a moderator{}", note.map(|n| format!(": {n}")).unwrap_or_default()),
        "bet" => format!("bet{on}"),
        "bet_win" => format!("bet won{on}"),
        "streak" => "streak bonus".to_string(),
        "catchup" => "catch-up points".to_string(),
        other => other.to_string(),
    }
}

/// An account's points history, newest first, as the points page lists it.
pub async fn history(db: &db::Db, account_id: i64, own: bool) -> Result<Vec<LedgerView>, AppError> {
    let rows = db::ledger_rows(db, account_id, HISTORY).await?;
    Ok(rows
        .into_iter()
        .map(|r| LedgerView {
            what: what(&r, own),
            date: r.round_date,
            amount: if r.amount > 0 {
                format!("+{}", templates::group_digits(r.amount))
            } else {
                templates::group_digits(r.amount)
            },
            gain: r.amount > 0,
        })
        .collect())
}

pub async fn points(State(state): State<AppState>, jar: CookieJar) -> Result<Response, AppError> {
    let Some(viewer) = current_viewer(&state, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    let open = Round::current(state.now()).key();
    if viewer.account.verified {
        crate::economy::collect(&state.db, viewer.account.id, &open).await?;
    }
    let rows = history(&state.db, viewer.account.id, true).await?;
    let balance = db::balance(&state.db, viewer.account.id).await?;
    let viewer = current_viewer(&state, &jar).await?.unwrap_or(viewer);
    Ok(templates::render(&PointsTemplate {
        pool: Pool::Global,
        test_mode: state.cfg.test_env(),
        impersonating: false,
        whose: None,
        balance_label: points_label(&balance),
        rows,
        stake: crate::economy::GUESS_STAKE,
        grant: crate::economy::DAILY_GRANT,
        bonus: crate::economy::SIGNUP_BONUS,
        user: Some(UserView::from(viewer)),
    }))
}

#[derive(Deserialize, Default)]
pub struct BoardQuery {
    /// "week" for gains from play over the last seven days.
    pub show: Option<String>,
}

/// `/g/leaderboard`: points first, then the most days won.
pub async fn leaderboard(state: State<AppState>, jar: CookieJar, q: Query<BoardQuery>) -> Result<Response, AppError> {
    leaderboard_in(Pool::Global, state, jar, q).await
}

/// `/leaderboard`: the 42 round has no points, so only the most days won.
pub async fn vienna_leaderboard(state: State<AppState>, jar: CookieJar, q: Query<BoardQuery>) -> Result<Response, AppError> {
    leaderboard_in(Pool::Vienna, state, jar, q).await
}

/// How long the leaderboard's tables are shown before they are worked out
/// again. They read every guess and every line of points there is, and at
/// thousands of players that is seconds, not milliseconds: done once in a
/// while, not on every visit. Fresh every time in tests.
const BOARD_FRESH: std::time::Duration =
    if cfg!(test) { std::time::Duration::ZERO } else { std::time::Duration::from_secs(30) };

/// The leaderboard's tables as everyone sees them, before the visitor's own
/// rows are marked: points (global round only), wins, and each player's
/// (login, display name, longest run, run still going, id).
struct Boards {
    points: Vec<db::PointsRow>,
    wins: Vec<db::LeaderboardRow>,
    runs: Vec<(String, String, i64, i64, i64)>,
}

type BoardKey = (bool, bool, String);

/// The leaderboard's tables, by round, all-time or week, and open round.
#[derive(Default)]
pub struct BoardCache {
    built: std::sync::Mutex<std::collections::HashMap<BoardKey, (std::time::Instant, std::sync::Arc<Boards>)>>,
    /// Held while tables are worked out, so only one request does it.
    building: tokio::sync::Mutex<()>,
}

/// The tables, from the cache while fresh. Out of date: one request works
/// them out again and the rest are shown the last ones meanwhile, so a busy
/// minute never queues up behind it. Never built: the rest wait for the
/// first.
async fn boards(state: &AppState, pool: Pool, week: bool, open: &str) -> Result<std::sync::Arc<Boards>, AppError> {
    let cache = &state.boards;
    let key: BoardKey = (pool == Pool::Global, week, open.to_string());
    let fresh = |at: &std::time::Instant| at.elapsed() < BOARD_FRESH;
    let last = cache.built.lock().unwrap_or_else(|e| e.into_inner()).get(&key).cloned();
    if let Some((at, b)) = &last {
        if fresh(at) {
            return Ok(b.clone());
        }
    }
    let _building = match (cache.building.try_lock(), &last) {
        (Ok(guard), _) => guard,
        (Err(_), Some((_, b))) => return Ok(b.clone()),
        (Err(_), None) => cache.building.lock().await,
    };
    if let Some((at, b)) = cache.built.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        if fresh(at) {
            return Ok(b.clone());
        }
    }
    let built = std::sync::Arc::new(build_boards(state, pool, week, open).await?);
    let mut all = cache.built.lock().unwrap_or_else(|e| e.into_inner());
    all.retain(|k, _| k.2 == open);
    all.insert(key, (std::time::Instant::now(), built.clone()));
    Ok(built)
}

async fn build_boards(state: &AppState, pool: Pool, week: bool, open: &str) -> Result<Boards, AppError> {
    let points = if pool == Pool::Global {
        let since = (chrono::Utc::now() - chrono::Duration::days(7)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        db::points_board(&state.db, week.then_some(since.as_str()), BOARD).await?
    } else {
        Vec::new()
    };
    let wins = db::leaderboard(&state.db, pool, open, BOARD).await?;
    // Longest streaks: each player's runs worked out from the days they
    // played, longest first, then the one still going.
    let open_day = chrono::NaiveDate::parse_from_str(open, "%Y-%m-%d").unwrap_or_else(|_| chrono::Utc::now().date_naive());
    let mut players: Vec<(i64, String, String, Vec<chrono::NaiveDate>)> = Vec::new();
    for (id, login, display_name, day) in db::play_days(&state.db, pool).await? {
        let Ok(day) = chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d") else { continue };
        match players.last_mut() {
            Some(p) if p.0 == id => p.3.push(day),
            _ => players.push((id, login, display_name, vec![day])),
        }
    }
    let mut runs: Vec<(String, String, i64, i64, i64)> = players
        .into_iter()
        .map(|(id, login, display_name, days)| {
            let (longest, current) = crate::stats::longest_and_current_streak(&days, open_day);
            (login, display_name, longest, current, id)
        })
        .collect();
    runs.sort_by(|a, b| b.2.cmp(&a.2).then(b.3.cmp(&a.3)).then(a.0.cmp(&b.0)));
    runs.truncate(BOARD as usize);
    Ok(Boards { points, wins, runs })
}

async fn leaderboard_in(
    pool: Pool,
    State(state): State<AppState>,
    jar: CookieJar,
    Query(q): Query<BoardQuery>,
) -> Result<Response, AppError> {
    let viewer = current_viewer(&state, &jar).await?;
    if let Some(away) = crate::handlers::outside_42(pool, viewer.as_ref(), "/g/leaderboard") {
        return Ok(away);
    }
    let week = q.show.as_deref() == Some("week");
    let open = Round::current(state.now()).key();
    crate::economy::settle_first(&state.db, pool, &open).await?;
    let boards = boards(&state, pool, week, &open).await?;
    let rows = boards.points.clone();
    let named = crate::handlers::names_visible(pool, viewer.as_ref());
    // Your own row is marked: by account in the global round, by 42 login
    // in the 42 one.
    let me_global = viewer.as_ref().map(|v| v.account.username.clone());
    let me_42 = viewer.as_ref().and_then(|v| v.intra.as_ref().map(|u| u.login.clone()));
    let wins: Vec<db::LeaderboardRow> = boards.wins.clone();
    let top_wins = wins.first().map_or(1, |r| r.wins.max(1));
    let win_ranks = templates::ranks(&wins.iter().map(|r| r.wins).collect::<Vec<_>>());
    let wins = wins
        .into_iter()
        .zip(win_ranks)
        .map(|(r, rank)| {
            let me = match pool {
                Pool::Global => me_global.as_deref() == Some(r.login.as_str()),
                Pool::Vienna => me_42.as_deref() == Some(r.login.as_str()),
            };
            let pct = r.wins * 100 / top_wins;
            let l = templates::LeaderView::new(r, named);
            PointsBoardRow { name: l.login, title: l.display_name, points: l.wins.to_string(), pct, me, rank }
        })
        .collect();
    let runs = boards.runs.clone();
    let top_run = runs.first().map_or(1, |r| r.2.max(1));
    let run_ranks = templates::ranks(&runs.iter().map(|r| r.2).collect::<Vec<_>>());
    let streaks: Vec<templates::StreakBoardRow> = runs
        .into_iter()
        .zip(run_ranks)
        .map(|((login, display_name, longest, current, id), rank)| {
            let me = match pool {
                Pool::Global => me_global.as_deref() == Some(login.as_str()),
                Pool::Vienna => me_42.as_deref() == Some(login.as_str()),
            };
            let shown = templates::LeaderView::new(db::LeaderboardRow { id, login, display_name, wins: 0 }, named);
            templates::StreakBoardRow { name: shown.login, title: shown.display_name, longest, current, pct: longest * 100 / top_run, me, rank }
        })
        .collect();
    let top_points = rows.first().map_or(1, |r| r.points.max(1));
    let point_ranks = templates::ranks(&rows.iter().map(|r| r.points).collect::<Vec<_>>());
    Ok(templates::render(&BoardTemplate {
        pool,
        test_mode: state.cfg.test_env(),
        impersonating: false,
        week,
        moderator: viewer.as_ref().is_some_and(|v| crate::moderation::is_moderator(&state, v)),
        wins,
        streaks,
        rows: rows
            .into_iter()
            .zip(point_ranks)
            .map(|(r, rank)| PointsBoardRow {
                rank,
                me: me_global.as_deref() == Some(r.username.as_str()),
                pct: (r.points.max(0) * 100 / top_points).clamp(0, 100),
                title: r.username.clone(),
                name: r.username,
                points: if week && r.points > 0 {
                    format!("+{}", templates::group_digits(r.points))
                } else {
                    templates::group_digits(r.points)
                },
            })
            .collect(),
        user: viewer.map(UserView::from),
    }))
}
