//! `/g/bets`: betting points on how the global round turns out (SPEC.md
//! section 9). The player picks a range for the winning number - or bets
//! nobody wins - sees its chance and odds, and bets at those odds. Below
//! that, a board of ready-made ranges, their bets on the open round, and
//! their slips from the last fortnight.
//!
//! Odds start from last week's picks, simulated once per open round and
//! kept until the next one opens, and move with the points already bet (see
//! `odds`). Today's guesses never go into them: they are secret until 12:42.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;

use crate::app::{AppError, AppState};
use crate::db::{self, Placed, Pool};
use crate::handlers::current_viewer;
use crate::limits;
use crate::odds::{self, Base, Market, Quote, Week};
use crate::round::Round;
use crate::templates::{self, BetsTemplate, Notice, OfferView, QuoteTemplate, QuoteView, SlipView, UserView};

/// How far back the page lists settled slips.
const SLIP_DAYS: i64 = 14;

/// Where the open round's odds start: from the cache if it was worked out
/// for this round, else simulated now from the week before and kept.
pub async fn todays_base(state: &AppState) -> Result<Arc<Base>, AppError> {
    let round = Round::current(state.now());
    let open = round.key();
    if let Some((r, base)) = state.odds_base.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if *r == open {
            return Ok(base.clone());
        }
    }
    let from = (round.date - chrono::Duration::days(odds::WEEK_DAYS)).format("%Y-%m-%d").to_string();
    let global = Week::from_tallies(&db::range_tallies(&state.db, Pool::Global, &from, &open).await?);
    let vienna = Week::from_tallies(&db::range_tallies(&state.db, Pool::Vienna, &from, &open).await?);
    let seed = odds::seed(&open);
    // A few thousand made-up rounds: off the async threads.
    let base = Arc::new(tokio::task::spawn_blocking(move || odds::base(&global, &vienna, seed)).await?);
    *state.odds_base.lock().unwrap_or_else(|e| e.into_inner()) = Some((open, base.clone()));
    Ok(base)
}

/// Stored bets as the odds see them; anything unreadable is left out.
fn parse_book(rows: &[(String, i64)]) -> Vec<(Market, i64)> {
    rows.iter().filter_map(|(k, s)| Market::parse(k).map(|m| (m, *s))).collect()
}

/// The open round's quotes for `markets`, as the book stands now.
pub async fn quotes(state: &AppState, markets: &[Market]) -> Result<Vec<Quote>, AppError> {
    let base = todays_base(state).await?;
    let book = parse_book(&db::book(&state.db, &Round::current(state.now()).key()).await?);
    Ok(markets.iter().map(|m| odds::quote(&base, &book, *m)).collect())
}

/// The round page's one line about bets: a couple of ranges and their odds.
pub async fn teaser(state: &AppState) -> Result<String, AppError> {
    let pick = [Market::Range { lo: 1, hi: Some(9) }, Market::Range { lo: 10, hi: None }, Market::Nobody];
    Ok(quotes(state, &pick)
        .await?
        .iter()
        .filter_map(|q| Some(format!("{} {}", q.market.label().trim_start_matches("winner "), odds::odds_label(q.odds_x100?))))
        .collect::<Vec<_>>()
        .join(" · "))
}

#[derive(Deserialize, Default)]
pub struct BetsQuery {
    pub msg: Option<String>,
    /// A market by key, as the board's links give it.
    pub m: Option<String>,
    /// A range as typed: from, and to (empty: or more).
    pub lo: Option<String>,
    pub hi: Option<String>,
    /// What the typed range is about: "most" for the most picked number,
    /// anything else for the winning number.
    pub on: Option<String>,
}

fn slip(b: db::Bet) -> SlipView {
    let market = Market::parse(&b.market).map_or_else(|| b.market.clone(), Market::label);
    let pays = odds::payout(b.stake, b.odds_x100);
    SlipView {
        date: templates::eu(&b.round_date),
        market,
        stake: b.stake,
        odds: odds::odds_label(b.odds_x100),
        result: b.result.clone().unwrap_or_else(|| "open".to_string()),
        // What came back once settled; what it would pay while open.
        amount: b.payout.unwrap_or(pays),
        net: b.payout.map(|p| p - b.stake),
    }
}

/// What the visitor picked: a board link (`m`), or the two boxes.
struct Pick {
    market: Option<Market>,
    /// Something was asked for, picked or typed.
    asked: bool,
    typed: bool,
}

impl Pick {
    fn from_query(q: &BetsQuery) -> Pick {
        let filled = |s: &Option<String>| s.as_deref().is_some_and(|s| !s.trim().is_empty());
        let typed = filled(&q.lo) || filled(&q.hi);
        let market = match (&q.m, typed) {
            (Some(key), _) => Market::parse(key),
            (None, true) => Market::from_inputs(q.lo.as_deref().unwrap_or(""), q.hi.as_deref().unwrap_or(""))
                .map(|m| if q.on.as_deref() == Some("most") { m.most() } else { m }),
            (None, false) => None,
        };
        Pick { market, asked: q.m.is_some() || typed, typed }
    }

    /// Why nothing could be quoted, if something was asked for.
    fn error(&self) -> Option<&'static str> {
        (self.asked && self.market.is_none()).then(|| crate::copy::notice("bet_market").map_or("", |(_, t)| t))
    }

    /// What the two boxes show: the range picked, or what was typed.
    fn inputs(&self, q: &BetsQuery) -> (String, String) {
        match (self.market, self.typed) {
            (Some(m), _) => m.inputs(),
            (None, true) => (q.lo.clone().unwrap_or_default(), q.hi.clone().unwrap_or_default()),
            (None, false) => (String::new(), String::new()),
        }
    }
}

fn offer_views(base: &Base, book: &[(Market, i64)], markets: Vec<Market>, picked: Option<Market>) -> Vec<OfferView> {
    let quotes: Vec<Quote> = markets.iter().map(|m| odds::quote(base, book, *m)).collect();
    let top = quotes.iter().map(|q| q.chance_permille).max().unwrap_or(1).max(1);
    quotes
        .into_iter()
        .map(|qt| OfferView {
            key: qt.market.key(),
            label: qt.market.label(),
            chance: odds::chance_label(qt.chance_permille),
            odds: qt.odds_x100.map(odds::odds_label),
            on: picked == Some(qt.market),
            pct: qt.chance_permille * 100 / top,
        })
        .collect()
}

fn quote_view(base: &Base, book: &[(Market, i64)], m: Market, taken: &[Market]) -> QuoteView {
    let qt = odds::quote(base, book, m);
    QuoteView {
        key: m.key(),
        label: m.label(),
        chance: odds::chance_label(qt.chance_permille),
        odds: qt.odds_x100.map(odds::odds_label),
        odds_x100: qt.odds_x100.unwrap_or(0),
        permille: qt.chance_permille,
        taken: taken.contains(&m),
    }
}

/// An account's bets: on the open round, and settled ones from the last
/// fortnight. Collects today's points first, so the stake room is right.
async fn slips(state: &AppState, viewer: Option<&db::Viewer>, round: &Round) -> Result<(Vec<db::Bet>, Vec<db::Bet>), AppError> {
    let Some(v) = viewer.filter(|v| v.account.verified) else {
        return Ok((Vec::new(), Vec::new()));
    };
    let open = round.key();
    crate::economy::collect(&state.db, v.account.id, &open).await?;
    let since = (round.date - chrono::Duration::days(SLIP_DAYS)).format("%Y-%m-%d").to_string();
    let all = db::bets_of(&state.db, v.account.id, &since).await?;
    Ok(all.into_iter().partition(|b| b.round_date == open))
}

pub async fn page(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(q): Query<BetsQuery>,
) -> Result<Response, AppError> {
    let viewer = current_viewer(&state, &jar).await?;
    let round = Round::current(state.now());
    let open = round.key();

    let pick = Pick::from_query(&q);

    let can_bet = viewer.as_ref().is_some_and(|v| v.account.verified);
    let (mine, past) = slips(&state, viewer.as_ref(), &round).await?;
    let taken: Vec<Market> = mine.iter().filter_map(|b| Market::parse(&b.market)).collect();
    let net: i64 = past.iter().filter_map(|b| b.payout.map(|p| p - b.stake)).sum();

    let base = todays_base(&state).await?;
    let book = parse_book(&db::book(&state.db, &open).await?);
    let quote = pick.market.map(|m| quote_view(&base, &book, m, &taken));
    let (lo, hi) = pick.inputs(&q);
    let notice = q
        .msg
        .as_deref()
        .and_then(crate::copy::notice)
        .map(|(kind, text)| if kind == "ok" { Notice::ok(text) } else { Notice::error(text) });
    // Re-read, so the header shows any points just collected.
    let viewer = current_viewer(&state, &jar).await?;

    Ok(templates::render(&BetsTemplate {
        pool: Pool::Global,
        test_mode: state.cfg.test_env(),
        impersonating: false,
        notice,
        round_key: open,
        close_time: round.deadline.with_timezone(&crate::round::TZ).format("%H:%M").to_string(),
        signed_in: viewer.is_some(),
        can_bet,
        lo,
        hi,
        nobody_picked: pick.market == Some(Market::Nobody),
        quote,
        pick_error: pick.error(),
        on_most: pick.market.is_some_and(Market::is_most) || (pick.market.is_none() && q.on.as_deref() == Some("most")),
        board: offer_views(&base, &book, odds::board(), pick.market),
        board_most: offer_views(&base, &book, odds::board_most(), pick.market),
        book_total: book.iter().map(|(_, s)| s).sum(),
        book_bets: book.len(),
        sims: base.sims,
        tax: odds::tax_label(),
        mine: mine.into_iter().map(slip).collect(),
        past: past.into_iter().map(slip).collect(),
        net_label: if net > 0 { format!("+{net}") } else { net.to_string() },
        user: viewer.map(UserView::from),
    }))
}

/// `/g/bets/quote`: just the quote box for what is typed, for app.js to
/// swap in while the visitor types. Same query as the page.
pub async fn quote(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(q): Query<BetsQuery>,
) -> Result<Response, AppError> {
    let viewer = current_viewer(&state, &jar).await?;
    let round = Round::current(state.now());
    let pick = Pick::from_query(&q);
    let (mine, _) = slips(&state, viewer.as_ref(), &round).await?;
    let taken: Vec<Market> = mine.iter().filter_map(|b| Market::parse(&b.market)).collect();
    let quote = match pick.market {
        Some(m) => {
            let base = todays_base(&state).await?;
            let book = parse_book(&db::book(&state.db, &round.key()).await?);
            Some(quote_view(&base, &book, m, &taken))
        }
        None => None,
    };
    let mut resp = templates::render(&QuoteTemplate {
        quote,
        pick_error: pick.error(),
        can_bet: viewer.as_ref().is_some_and(|v| v.account.verified),
        round_key: round.key(),
        tax: odds::tax_label(),
    });
    // The odds move with every bet: never from a cache.
    resp.headers_mut().insert(axum::http::header::CACHE_CONTROL, axum::http::HeaderValue::from_static("no-store"));
    Ok(resp)
}

#[derive(Deserialize)]
pub struct BetForm {
    pub market: String,
    pub stake: String,
    /// The odds the visitor was shown, in hundredths: the bet is placed at
    /// these or better, or not at all.
    pub odds: i64,
    /// The round the page was for.
    pub round: String,
}

/// Back to the bets page with a message, and the range still picked.
fn back(code: &str, market: Option<Market>) -> Response {
    let pick = market.map(|m| format!("&m={}", m.key())).unwrap_or_default();
    Redirect::to(&format!("/g/bets?msg={code}{pick}")).into_response()
}

pub async fn place(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<BetForm>,
) -> Result<Response, AppError> {
    let Some(viewer) = current_viewer(&state, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    if !viewer.account.verified {
        return Ok(back("unverified", None));
    }
    let Some(market) = Market::parse(&form.market) else {
        return Ok(back("bet_market", None));
    };
    let open = Round::current(state.now()).key();
    if form.round != open {
        return Ok(back("bet_closed", Some(market)));
    }
    let Ok(stake) = form.stake.trim().parse::<i64>() else {
        return Ok(back("bet_cap", Some(market)));
    };
    if !state.limits.take(&format!("bet:{}", viewer.account.id), limits::BETS_PER_ACCOUNT) {
        return Ok(back("bet_slow", Some(market)));
    }
    let base = todays_base(&state).await?;
    crate::economy::collect(&state.db, viewer.account.id, &open).await?;
    // Priced against the book as it stands when the bet goes in: at the
    // odds shown, or better if they lengthened since.
    let shown = form.odds;
    let price = |rows: &[(String, i64)]| odds::quote(&base, &parse_book(rows), market).odds_x100.filter(|o| *o >= shown);
    let code = match db::place_bet(&state.db, viewer.account.id, &open, &market.key(), stake, price).await? {
        Placed::In => "bet_placed",
        Placed::Already => "bet_twice",
        Placed::BadStake => "bet_cap",
        Placed::Broke => "broke",
        Placed::Closed => "bet_closed",
        Placed::Moved => "bet_odds",
    };
    Ok(back(code, Some(market)))
}

#[cfg(test)]
mod tests {
    use axum::extract::{Form, Query, State};
    use axum_extra::extract::cookie::CookieJar;

    use super::*;
    use crate::db::{test_support::temp_db, Db};
    use crate::economy::{collect, settle_all, GUESS_STAKE};
    use crate::testkit::{app, body, jar, location, stranger, student};

    async fn player(db: &Db, name: &str) -> i64 {
        stranger(db, name, name).await;
        sqlx::query("UPDATE accounts SET verified_at = 'x' WHERE username = ?1").bind(name).execute(db).await.unwrap();
        db::viewer_for_session(db, name).await.unwrap().unwrap().account.id
    }

    async fn plays(db: &Db, who: i64, round: &str, value: i64) {
        collect(db, who, round).await.unwrap();
        db::lock_global_guess(db, round, who, value, GUESS_STAKE).await.unwrap();
    }

    async fn bet(db: &Db, who: i64, round: &str, market: &str, stake: i64, odds: i64) -> Placed {
        db::place_bet(db, who, round, market, stake, |_| Some(odds)).await.unwrap()
    }

    #[tokio::test]
    async fn a_bet_takes_any_stake_up_to_the_balance_once() {
        let t = temp_db().await;
        let ann = player(&t.db, "ann").await;
        collect(&t.db, ann, "2026-10-21").await.unwrap();
        let start = db::balance(&t.db, ann).await.unwrap();

        assert_eq!(bet(&t.db, ann, "2026-10-21", "range:1:9", 50, 135).await, Placed::In);
        assert_eq!(db::balance(&t.db, ann).await.unwrap(), start - 50);
        assert_eq!(bet(&t.db, ann, "2026-10-21", "range:1:9", 10, 135).await, Placed::Already, "one per range");
        assert_eq!(bet(&t.db, ann, "2026-10-21", "range:10:", 0, 260).await, Placed::BadStake);
        assert_eq!(bet(&t.db, ann, "2026-10-21", "range:10:", -5, 260).await, Placed::BadStake);
        // No cap but the balance: all of it is fine, a point more is not.
        let left = start - 50;
        assert_eq!(bet(&t.db, ann, "2026-10-21", "none", left + 1, 980).await, Placed::Broke);
        assert_eq!(bet(&t.db, ann, "2026-10-21", "range:10:", left, 260).await, Placed::In, "everything on it");
        assert_eq!(db::balance(&t.db, ann).await.unwrap(), 0);
        assert_eq!(bet(&t.db, ann, "2026-10-21", "most:1:3", 1, 300).await, Placed::Broke);
    }

    #[tokio::test]
    async fn without_points_nothing_is_placed() {
        let t = temp_db().await;
        let ann = player(&t.db, "ann").await;
        assert_eq!(bet(&t.db, ann, "2026-10-21", "none", 10, 980).await, Placed::Broke);
        assert!(db::bets_of(&t.db, ann, "").await.unwrap().is_empty());
        assert_eq!(db::balance(&t.db, ann).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn bets_are_paid_at_their_locked_odds_when_the_round_settles() {
        let t = temp_db().await;
        let (ann, bob, cat) = (player(&t.db, "ann").await, player(&t.db, "bob").await, player(&t.db, "cat").await);
        for (who, v) in [(ann, 1), (bob, 1), (cat, 7)] {
            plays(&t.db, who, "2026-10-21", v).await;
        }
        // Winner will be 7.
        assert_eq!(bet(&t.db, ann, "2026-10-21", "range:1:9", 50, 135).await, Placed::In);
        assert_eq!(bet(&t.db, ann, "2026-10-21", "range:10:", 20, 260).await, Placed::In);
        assert_eq!(bet(&t.db, bob, "2026-10-21", "none", 10, 980).await, Placed::In);
        let (a0, b0) = (db::balance(&t.db, ann).await.unwrap(), db::balance(&t.db, bob).await.unwrap());

        settle_all(&t.db, "2026-10-22").await.unwrap();
        assert_eq!(db::balance(&t.db, ann).await.unwrap(), a0 + 67, "50 at ×1.35 pays 67; over loses");
        assert_eq!(db::balance(&t.db, bob).await.unwrap(), b0, "nobody-wins lost");
        let slips = db::bets_of(&t.db, ann, "").await.unwrap();
        let won = slips.iter().find(|b| b.market == "range:1:9").unwrap();
        assert_eq!((won.result.as_deref(), won.payout), (Some("won"), Some(67)));
        let lost = slips.iter().find(|b| b.market == "range:10:").unwrap();
        assert_eq!((lost.result.as_deref(), lost.payout), (Some("lost"), Some(0)));
        // Settling again pays nothing more, and the round takes no more bets.
        settle_all(&t.db, "2026-10-22").await.unwrap();
        assert_eq!(db::balance(&t.db, ann).await.unwrap(), a0 + 67);
        assert_eq!(bet(&t.db, cat, "2026-10-21", "none", 10, 980).await, Placed::Closed);
    }

    #[tokio::test]
    async fn nobody_wins_pays_and_under_over_lose_when_every_number_collides() {
        let t = temp_db().await;
        let (ann, bob) = (player(&t.db, "ann").await, player(&t.db, "bob").await);
        plays(&t.db, ann, "2026-10-21", 3).await;
        plays(&t.db, bob, "2026-10-21", 3).await;
        bet(&t.db, ann, "2026-10-21", "none", 10, 980).await;
        bet(&t.db, bob, "2026-10-21", "range:1:4", 10, 180).await;
        let (a0, b0) = (db::balance(&t.db, ann).await.unwrap(), db::balance(&t.db, bob).await.unwrap());
        settle_all(&t.db, "2026-10-22").await.unwrap();
        assert_eq!(db::balance(&t.db, ann).await.unwrap(), a0 + 98);
        assert_eq!(db::balance(&t.db, bob).await.unwrap(), b0);
    }

    #[tokio::test]
    async fn most_picked_bets_settle_on_the_most_picked_number_lowest_on_a_tie() {
        let t = temp_db().await;
        let who: Vec<i64> = futures_join(&t.db, &["a", "b", "c", "d", "e", "f"]).await;
        // 3 three times, 1 twice, 8 once: the winner is 8, the most picked 3.
        for (p, v) in who.iter().zip([3, 3, 3, 1, 1, 8]) {
            plays(&t.db, *p, "2026-10-21", v).await;
        }
        let ann = who[0];
        for (m, o) in [("most:3:3", 400), ("most:1:2", 300), ("range:8:8", 900), ("most:4:", 500)] {
            assert_eq!(bet(&t.db, ann, "2026-10-21", m, 10, o).await, Placed::In, "{m}");
        }
        let a0 = db::balance(&t.db, ann).await.unwrap();
        settle_all(&t.db, "2026-10-22").await.unwrap();
        let slips = db::bets_of(&t.db, ann, "").await.unwrap();
        let result = |m: &str| slips.iter().find(|b| b.market == m).unwrap().result.clone().unwrap();
        assert_eq!(result("most:3:3"), "won");
        assert_eq!(result("most:1:2"), "lost");
        assert_eq!(result("most:4:"), "lost");
        assert_eq!(result("range:8:8"), "won", "the winner book is judged on its own");
        assert_eq!(db::balance(&t.db, ann).await.unwrap(), a0 + 40 + 90);

        // A tie for most picked goes to the lowest number.
        for (p, v) in who.iter().take(4).zip([6, 6, 2, 2]) {
            plays(&t.db, *p, "2026-10-22", v).await;
        }
        bet(&t.db, ann, "2026-10-22", "most:2:2", 10, 300).await;
        bet(&t.db, ann, "2026-10-22", "most:6:6", 10, 300).await;
        settle_all(&t.db, "2026-10-23").await.unwrap();
        let slips = db::bets_of(&t.db, ann, "2026-10-22").await.unwrap();
        let result = |m: &str| slips.iter().find(|b| b.market == m).unwrap().result.clone().unwrap();
        assert_eq!((result("most:2:2").as_str(), result("most:6:6").as_str()), ("won", "lost"));
    }

    async fn futures_join(db: &Db, names: &[&str]) -> Vec<i64> {
        let mut v = Vec::new();
        for n in names {
            v.push(player(db, n).await);
        }
        v
    }

    #[tokio::test]
    async fn a_round_nobody_played_refunds_its_bets() {
        let t = temp_db().await;
        let ann = player(&t.db, "ann").await;
        collect(&t.db, ann, "2026-10-21").await.unwrap();
        bet(&t.db, ann, "2026-10-21", "none", 40, 980).await;
        let before = db::balance(&t.db, ann).await.unwrap();
        settle_all(&t.db, "2026-10-22").await.unwrap();
        assert_eq!(db::balance(&t.db, ann).await.unwrap(), before + 40);
        assert_eq!(db::bets_of(&t.db, ann, "").await.unwrap()[0].result.as_deref(), Some("void"));
    }

    #[tokio::test]
    async fn calling_a_round_off_or_a_ban_refunds_open_bets() {
        let t = temp_db().await;
        let (ann, cheat) = (player(&t.db, "ann").await, player(&t.db, "cheat").await);
        plays(&t.db, ann, "2026-10-21", 4).await;
        plays(&t.db, cheat, "2026-10-21", 2).await;
        bet(&t.db, ann, "2026-10-21", "range:1:4", 30, 180).await;
        bet(&t.db, cheat, "2026-10-21", "range:5:", 30, 180).await;
        let c0 = db::balance(&t.db, cheat).await.unwrap();
        db::ban_account(&t.db, cheat, "alts", "2026-10-21").await.unwrap();
        assert_eq!(db::balance(&t.db, cheat).await.unwrap(), c0 + 30 + GUESS_STAKE, "bet and stake back");

        let a0 = db::balance(&t.db, ann).await.unwrap();
        assert!(db::void_round(&t.db, "2026-10-21").await.unwrap());
        assert_eq!(db::balance(&t.db, ann).await.unwrap(), a0 + 30 + GUESS_STAKE);
        let refunds: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM ledger WHERE kind = 'refund' AND ref LIKE 'bet:%'")
            .fetch_one(&t.db)
            .await
            .unwrap();
        assert_eq!(refunds.0, 2, "each bet refunded exactly once");
    }

    #[tokio::test]
    async fn a_bet_refused_by_its_price_takes_nothing() {
        let t = temp_db().await;
        let ann = player(&t.db, "ann").await;
        collect(&t.db, ann, "2026-10-21").await.unwrap();
        let start = db::balance(&t.db, ann).await.unwrap();
        assert_eq!(db::place_bet(&t.db, ann, "2026-10-21", "none", 10, |_| None).await.unwrap(), Placed::Moved);
        assert_eq!(db::balance(&t.db, ann).await.unwrap(), start);
        assert!(db::bets_of(&t.db, ann, "").await.unwrap().is_empty());
        // The price sees the bets already in.
        bet(&t.db, ann, "2026-10-21", "range:1:4", 30, 200).await;
        let mut seen = Vec::new();
        db::place_bet(&t.db, ann, "2026-10-21", "none", 10, |rows| {
            seen = rows.to_vec();
            Some(900)
        })
        .await
        .unwrap();
        assert_eq!(seen, vec![("range:1:4".to_string(), 30)]);
    }

    #[tokio::test]
    async fn moving_the_clock_back_takes_back_what_the_future_paid() {
        let (_t, state) = app(&["megardes"]).await;
        student(&state.db, 1001, "megardes", "adm").await;
        let (ann, bob) = (player(&state.db, "ann").await, player(&state.db, "bob").await);
        let today = Round::current(state.now()).key();
        plays(&state.db, ann, &today, 4).await;
        plays(&state.db, bob, &today, 9).await;
        bet(&state.db, ann, &today, "range:1:5", 30, 200).await;
        let (a0, b0) = (db::balance(&state.db, ann).await.unwrap(), db::balance(&state.db, bob).await.unwrap());
        let clock = |shift: &str| {
            let form = Form(crate::handlers::ClockForm { shift: shift.into() });
            crate::handlers::admin_clock(State(state.clone()), jar("adm"), form)
        };

        // Past the deadline: the round settles, ann wins the pot and her bet,
        // and both pick up tomorrow's daily points.
        clock("deadline").await.unwrap();
        let tomorrow = Round::current(state.now()).key();
        settle_all(&state.db, &tomorrow).await.unwrap();
        collect(&state.db, ann, &tomorrow).await.unwrap();
        collect(&state.db, bob, &tomorrow).await.unwrap();
        assert_eq!(db::balance(&state.db, ann).await.unwrap(), a0 + 20 + 60 + crate::economy::DAILY_GRANT);
        assert!(db::pot(&state.db, &today).await.unwrap().is_some());

        // Back to now: all of it goes back, the bet is open again.
        clock("reset").await.unwrap();
        assert_eq!(Round::current(state.now()).key(), today);
        assert_eq!(db::balance(&state.db, ann).await.unwrap(), a0);
        assert_eq!(db::balance(&state.db, bob).await.unwrap(), b0);
        assert!(db::pot(&state.db, &today).await.unwrap().is_none());
        assert_eq!(db::bets_of(&state.db, ann, &today).await.unwrap()[0].result, None);

        // And when the round really closes, it settles once, as normal.
        clock("deadline").await.unwrap();
        settle_all(&state.db, &tomorrow).await.unwrap();
        settle_all(&state.db, &tomorrow).await.unwrap();
        assert_eq!(db::balance(&state.db, ann).await.unwrap(), a0 + 20 + 60);
    }

    // ------------------------------------------------------------- pages

    async fn bets_page_q(state: &AppState, token: Option<&str>, q: BetsQuery) -> String {
        let jar = token.map(jar).unwrap_or_default();
        body(page(State(state.clone()), jar, Query(q)).await.unwrap()).await
    }

    async fn bets_page(state: &AppState, token: Option<&str>, msg: Option<&str>) -> String {
        bets_page_q(state, token, BetsQuery { msg: msg.map(str::to_string), ..Default::default() }).await
    }

    fn picked(m: &str) -> BetsQuery {
        BetsQuery { m: Some(m.into()), ..Default::default() }
    }

    fn form(state: &AppState, market: &str, stake: &str, odds: i64) -> Form<BetForm> {
        Form(BetForm {
            market: market.into(),
            stake: stake.into(),
            odds,
            round: Round::current(state.now()).key(),
        })
    }

    async fn odds_now(state: &AppState, market: &str) -> Option<i64> {
        quotes(state, &[Market::parse(market).unwrap()]).await.unwrap()[0].odds_x100
    }

    async fn odds_for(state: &AppState, market: &str) -> i64 {
        odds_now(state, market).await.unwrap()
    }

    /// Last week, as a crowd of 20 a day picking mostly low numbers.
    async fn last_week(state: &AppState) {
        let today = Round::current(state.now()).date;
        let mut n = 0;
        for d in 1..=7 {
            let day = (today - chrono::Duration::days(d)).format("%Y-%m-%d").to_string();
            for (i, v) in [1, 1, 2, 2, 3, 3, 3, 4, 5, 5, 6, 7, 8, 9, 11, 13, 17, 23, 42, 100].into_iter().enumerate() {
                let who = player(&state.db, &format!("p{d}x{i}")).await;
                db::insert_guess(&state.db, Pool::Global, &day, who, v, true).await.unwrap();
                n += 1;
            }
        }
        assert_eq!(n, 140);
    }

    #[tokio::test]
    async fn the_page_shows_the_board_and_a_picked_range_takes_a_bet() {
        let (_t, state) = app(&[]).await;
        player(&state.db, "ann").await;
        let out = bets_page(&state, None, None).await;
        assert!(out.contains("winner exactly 1") && out.contains("winner 50 or more") && out.contains("nobody wins"));
        assert!(out.contains(r#"href="/g/bets?m=range:6:9""#));
        assert!(!out.contains(r#"action="/g/bet""#), "signed out: no forms");
        let html = bets_page(&state, Some("ann"), None).await;
        assert!(!html.contains(r#"action="/g/bet""#), "nothing picked yet");
        assert!(html.contains("stake as much as you like, up to your points"));

        let html = bets_page_q(&state, Some("ann"), picked("range:5:12")).await;
        assert_eq!(html.matches(r#"action="/g/bet""#).count(), 1);
        assert!(html.contains("winner 5 to 12") && html.contains(r#"value="5""#) && html.contains(r#"value="12""#));

        let o = odds_for(&state, "range:5:12").await;
        let resp = place(State(state.clone()), jar("ann"), form(&state, "range:5:12", "40", o)).await.unwrap();
        assert_eq!(location(&resp), "/g/bets?msg=bet_placed&m=range:5:12");
        let html = bets_page_q(&state, Some("ann"), BetsQuery { msg: Some("bet_placed".into()), m: Some("range:5:12".into()), ..Default::default() }).await;
        assert!(html.contains("bet placed · settled at 12:42"));
        assert!(html.contains("your bets today"));
        assert!(html.contains("placed &middot; one bet per range a round") && !html.contains(r#"action="/g/bet""#));
        assert!(html.contains("on the book: 40 pts, 1 bet"));
        assert!(html.contains(&format!("<td class=\"num b win\">{}</td>", odds::payout(40, o))));
    }

    #[tokio::test]
    async fn a_typed_range_is_quoted_and_a_bad_one_is_explained() {
        let (_t, state) = app(&[]).await;
        player(&state.db, "ann").await;
        let typed = |lo: &str, hi: &str| BetsQuery { lo: Some(lo.into()), hi: Some(hi.into()), ..Default::default() };
        let html = bets_page_q(&state, Some("ann"), typed("7", "")).await;
        assert!(html.contains("winner 7 or more") && html.contains(r#"action="/g/bet""#));
        let html = bets_page_q(&state, Some("ann"), typed("3", "3")).await;
        assert!(html.contains("winner exactly 3"));
        let html = bets_page_q(&state, Some("ann"), typed("12", "5")).await;
        assert!(html.contains("pick a range: whole numbers from 1, low to high"));
        assert!(html.contains(r#"value="12""#), "what was typed stays");
        assert!(!html.contains(r#"action="/g/bet""#));
        // Any winner at all: so likely that after the cut it pays less than it costs.
        let html = bets_page_q(&state, Some("ann"), typed("1", "")).await;
        assert!(html.contains("too safe to bet on") && !html.contains(r#"action="/g/bet""#));
    }

    #[tokio::test]
    async fn the_quote_box_alone_follows_what_is_typed() {
        let (_t, state) = app(&[]).await;
        player(&state.db, "ann").await;
        let ask = |lo: &str, hi: &str, token: Option<&str>| {
            let q = BetsQuery { lo: Some(lo.into()), hi: Some(hi.into()), ..Default::default() };
            let jar = token.map(jar).unwrap_or_default();
            quote(State(state.clone()), jar, Query(q))
        };
        let resp = ask("5", "12", Some("ann")).await.unwrap();
        assert_eq!(resp.headers()["cache-control"], "no-store", "odds move: never cached");
        let html = body(resp).await;
        assert!(html.contains("winner 5 to 12") && html.contains(r#"action="/g/bet""#));
        assert!(!html.contains("<header") && !html.contains("<html"), "just the box");
        let o = odds_for(&state, "range:5:12").await;
        assert!(html.contains(&format!(r#"name="odds" value="{o}""#)));
        // What it pays is worked out from the stake as it is typed, not for 10.
        assert!(html.contains(&format!(r#"<dt>if it wins</dt><dd class="pays" data-odds="{o}">"#)) && !html.contains("10 pays"));
        // Signed out: the odds, no form.
        let html = body(ask("5", "12", None).await.unwrap()).await;
        assert!(html.contains("winner 5 to 12") && !html.contains(r#"action="/g/bet""#));
        // Half typed or backwards: said so. Nothing typed: nothing.
        let html = body(ask("12", "5", Some("ann")).await.unwrap()).await;
        assert!(html.contains("pick a range: whole numbers from 1, low to high"));
        assert!(body(ask("", "", Some("ann")).await.unwrap()).await.trim().is_empty());
        assert!(body(ask("1", "", Some("ann")).await.unwrap()).await.contains("too safe to bet on"));
    }

    #[tokio::test]
    async fn a_bet_is_placed_at_the_odds_shown_or_better_or_not_at_all() {
        let (_t, state) = app(&[]).await;
        let ann = player(&state.db, "ann").await;
        let o = odds_for(&state, "none").await;
        for (f, code) in [
            (form(&state, "none", "10", o + 5), "bet_odds&m=none"),
            (form(&state, "range:9:3", "10", o), "bet_market"),
            (form(&state, "under:7;", "10", o), "bet_market"),
            (form(&state, "none", "lots", o), "bet_cap&m=none"),
            (form(&state, "range:1:", "10", 105), "bet_odds&m=range:1:"),
            (Form(BetForm { market: "none".into(), stake: "10".into(), odds: o, round: "2000-01-01".into() }), "bet_closed&m=none"),
        ] {
            let resp = place(State(state.clone()), jar("ann"), f).await.unwrap();
            assert_eq!(location(&resp), format!("/g/bets?msg={code}"));
        }
        assert!(db::bets_of(&state.db, ann, "").await.unwrap().is_empty());
        // Shown worse odds than the book now gives: placed at the better ones.
        let resp = place(State(state.clone()), jar("ann"), form(&state, "none", "10", o - 50)).await.unwrap();
        assert_eq!(location(&resp), "/g/bets?msg=bet_placed&m=none");
        assert_eq!(db::bets_of(&state.db, ann, "").await.unwrap()[0].odds_x100, o);
        // Not signed in: to sign in. Not confirmed: told so.
        let resp = place(State(state.clone()), CookieJar::new(), form(&state, "none", "10", o)).await.unwrap();
        assert_eq!(location(&resp), "/login");
        stranger(&state.db, "new", "new").await;
        let resp = place(State(state.clone()), jar("new"), form(&state, "none", "10", o)).await.unwrap();
        assert_eq!(location(&resp), "/g/bets?msg=unverified");
    }

    #[tokio::test]
    async fn odds_come_from_last_week_and_move_as_people_bet() {
        let (_t, state) = app(&[]).await;
        last_week(&state).await;
        let base = todays_base(&state).await.unwrap();
        assert_eq!((base.picks, base.players, base.sims > 0), (140, 20, true));
        let html = bets_page(&state, None, None).await;
        assert!(html.contains("from last week&rsquo;s picks, replayed 4000&times;"));

        let (ann, bob) = (player(&state.db, "ann").await, player(&state.db, "bob").await);
        let (mid, high) = ("range:5:12", "range:1:4");
        let (mid0, high0) = (odds_for(&state, mid).await, odds_for(&state, high).await);
        place(State(state.clone()), jar("ann"), form(&state, mid, "50", mid0)).await.unwrap();
        let mid1 = odds_for(&state, mid).await;
        assert!(mid1 < mid0, "backed: shorter, {mid0} -> {mid1}");
        assert_eq!(odds_for(&state, high).await, high0, "not backed: never longer than it started");
        // Bob is shown the new odds; the old ones are gone.
        let resp = place(State(state.clone()), jar("bob"), form(&state, mid, "50", mid0)).await.unwrap();
        assert_eq!(location(&resp), "/g/bets?msg=bet_odds&m=range:5:12");
        place(State(state.clone()), jar("bob"), form(&state, mid, "50", mid1)).await.unwrap();
        // Each keeps the odds they bet at.
        assert_eq!(db::bets_of(&state.db, ann, "").await.unwrap()[0].odds_x100, mid0);
        assert_eq!(db::bets_of(&state.db, bob, "").await.unwrap()[0].odds_x100, mid1);
        assert!(odds_for(&state, mid).await < mid1);
        // The odds on the round page follow the book too.
        let teaser = teaser(&state).await.unwrap();
        assert!(teaser.contains("10 or more ×") && teaser.contains("nobody wins ×"), "{teaser}");
    }

    #[tokio::test]
    async fn the_simulation_runs_once_a_round() {
        let (_t, state) = app(&[]).await;
        let first = todays_base(&state).await.unwrap();
        // Rounds closing in the past do not change today's starting odds once set.
        let bob = player(&state.db, "bob").await;
        let yesterday = Round::current(state.now()).previous_date().format("%Y-%m-%d").to_string();
        db::insert_guess(&state.db, Pool::Global, &yesterday, bob, 40, true).await.unwrap();
        assert!(Arc::ptr_eq(&todays_base(&state).await.unwrap(), &first), "cached for the round");
        *state.odds_base.lock().unwrap() = None;
        assert_eq!(todays_base(&state).await.unwrap().picks, 1, "worked out again: yesterday counts");
    }

    #[tokio::test]
    async fn scripts_are_slowed_down() {
        let (_t, state) = app(&[]).await;
        player(&state.db, "ann").await;
        let o = odds_for(&state, "none").await;
        let mut last = String::new();
        for _ in 0..31 {
            let resp = place(State(state.clone()), jar("ann"), form(&state, "none", "1", o)).await.unwrap();
            last = location(&resp).to_string();
        }
        assert_eq!(last, "/g/bets?msg=bet_slow&m=none");
    }
    #[tokio::test]
    async fn the_global_round_page_points_to_the_bets_and_the_42_round_does_not() {
        let (_t, state) = app(&[]).await;
        student(&state.db, 1001, "megardes", "s").await;
        let q = || Query(crate::handlers::IndexQuery { msg: None, n: None });
        let global = body(crate::handlers::global_index(State(state.clone()), jar("s"), q()).await.unwrap()).await;
        assert!(global.contains(r#"<a href="/g/bets">all bets &rarr;</a>"#));
        assert!(global.contains("10 or more ×"), "1 to 9 is too safe to offer with no history");
        assert!(global.contains(r#"href="/g/bets">bets</a>"#), "a tab in the global round");
        let vienna = body(crate::handlers::index(State(state.clone()), jar("s"), q()).await.unwrap()).await;
        assert!(!vienna.contains("/g/bets"), "no bets in the 42 round");
    }

    #[tokio::test]
    async fn settled_bets_show_in_the_points_history() {
        let (_t, state) = app(&[]).await;
        let ann = player(&state.db, "ann").await;
        let day = Round::current(state.now()).previous_date().format("%Y-%m-%d").to_string();
        plays(&state.db, ann, &day, 3).await;
        bet(&state.db, ann, &day, "range:1:4", 10, 180).await;
        settle_all(&state.db, &Round::current(state.now()).key()).await.unwrap();
        let html = body(crate::points_pages::points(State(state.clone()), jar("ann")).await.unwrap()).await;
        assert!(html.contains(">bet<") && html.contains(">bet won<") && html.contains("+18"), "{html}");
        let page = bets_page(&state, Some("ann"), None).await;
        assert!(page.contains("past bets") && page.contains("net +8"));
    }
}
