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

/// What a ledger line was, in words.
fn what(kind: &str, note: Option<&str>) -> String {
    match kind {
        "signup" => "welcome points".to_string(),
        "grant" => "daily points".to_string(),
        "stake" => "your guess".to_string(),
        "pot" => "won the pot".to_string(),
        "refund" => format!("refund{}", note.map(|n| format!(" ({n})")).unwrap_or_default()),
        "adjust" => format!("adjusted by a moderator{}", note.map(|n| format!(": {n}")).unwrap_or_default()),
        "bet" => "bet".to_string(),
        "bet_win" => "bet won".to_string(),
        "streak" => "streak bonus".to_string(),
        other => other.to_string(),
    }
}

pub async fn points(State(state): State<AppState>, jar: CookieJar) -> Result<Response, AppError> {
    let Some(viewer) = current_viewer(&state, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    let open = Round::current(state.now()).key();
    if viewer.account.verified {
        crate::economy::collect(&state.db, viewer.account.id, &open).await?;
    }
    let rows = db::ledger_rows(&state.db, viewer.account.id, HISTORY).await?;
    let balance = db::balance(&state.db, viewer.account.id).await?;
    let viewer = current_viewer(&state, &jar).await?.unwrap_or(viewer);
    Ok(templates::render(&PointsTemplate {
        pool: Pool::Global,
        test_mode: state.cfg.test_env(),
        impersonating: false,
        balance_label: points_label(&balance),
        rows: rows
            .into_iter()
            .map(|r| LedgerView {
                date: r.round_date,
                what: what(&r.kind, r.note.as_deref()),
                amount: if r.amount > 0 {
                    format!("+{}", templates::group_digits(r.amount))
                } else {
                    templates::group_digits(r.amount)
                },
                gain: r.amount > 0,
            })
            .collect(),
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
    let rows = if pool == Pool::Global {
        let since = (chrono::Utc::now() - chrono::Duration::days(7)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        db::points_board(&state.db, week.then_some(since.as_str()), BOARD).await?
    } else {
        Vec::new()
    };
    let open = Round::current(state.now()).key();
    let named = crate::handlers::names_visible(pool, viewer.as_ref());
    let wins = db::leaderboard(&state.db, pool, &open, BOARD)
        .await?
        .into_iter()
        .map(|r| templates::LeaderView::new(r, named))
        .collect();
    Ok(templates::render(&BoardTemplate {
        pool,
        test_mode: state.cfg.test_env(),
        impersonating: false,
        week,
        wins,
        rows: rows
            .into_iter()
            .map(|r| PointsBoardRow {
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
