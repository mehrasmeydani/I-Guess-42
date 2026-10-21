//! `/g/bets`: betting points on how the global round turns out (SPEC.md
//! section 9). Today's markets with their chances and odds, the visitor's
//! bets on the open round, and their slips from the last fortnight.
//!
//! Odds are worked out once per open round, from closed rounds only, and
//! kept until the next one opens: they never move with bets or guesses made
//! today, which are secret until 12:42 and must not leak through prices.

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;

use crate::app::{AppError, AppState};
use crate::db::{self, Placed, Pool};
use crate::handlers::current_viewer;
use crate::limits;
use crate::odds::{self, Market, Offer};
use crate::round::Round;
use crate::templates::{self, BetsTemplate, Notice, OfferView, SlipView, UserView};

/// How far back the page lists settled slips.
const SLIP_DAYS: i64 = 14;

/// The open round's offers: from the cache if they were worked out for this
/// round, else worked out now and kept.
pub async fn todays_offers(state: &AppState) -> Result<Vec<Offer>, AppError> {
    let open = Round::current(state.now()).key();
    if let Some((round, offers)) = state.offers.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if *round == open {
            return Ok(offers.clone());
        }
    }
    let limit = odds::HISTORY as i64;
    let global = db::winner_history(&state.db, Pool::Global, &open, limit).await?;
    let vienna = db::winner_history(&state.db, Pool::Vienna, &open, limit).await?;
    let offers = odds::offers(&global, &vienna);
    *state.offers.lock().unwrap_or_else(|e| e.into_inner()) = Some((open, offers.clone()));
    Ok(offers)
}

#[derive(Deserialize, Default)]
pub struct BetsQuery {
    pub msg: Option<String>,
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

pub async fn page(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(q): Query<BetsQuery>,
) -> Result<Response, AppError> {
    let viewer = current_viewer(&state, &jar).await?;
    let round = Round::current(state.now());
    let open = round.key();
    let offers = todays_offers(&state).await?;

    let can_bet = viewer.as_ref().is_some_and(|v| v.account.verified);
    let (mine, past) = match viewer.as_ref().filter(|v| v.account.verified) {
        Some(v) => {
            crate::economy::collect(&state.db, v.account.id, &open).await?;
            let since = (round.date - chrono::Duration::days(SLIP_DAYS)).format("%Y-%m-%d").to_string();
            let all = db::bets_of(&state.db, v.account.id, &since).await?;
            let (today, before): (Vec<_>, Vec<_>) = all.into_iter().partition(|b| b.round_date == open);
            (today, before)
        }
        None => (Vec::new(), Vec::new()),
    };
    let taken: Vec<String> = mine.iter().map(|b| b.market.clone()).collect();
    let staked: i64 = mine.iter().filter(|b| b.result.is_none()).map(|b| b.stake).sum();
    let net: i64 = past.iter().filter_map(|b| b.payout.map(|p| p - b.stake)).sum();
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
        offers: offers
            .iter()
            .map(|o| OfferView {
                key: o.market.key(),
                label: o.market.label(),
                chance: o.chance_pct,
                odds: odds::odds_label(o.odds_x100),
                odds_x100: o.odds_x100,
                taken: taken.contains(&o.market.key()),
            })
            .collect(),
        mine: mine.into_iter().map(slip).collect(),
        past: past.into_iter().map(slip).collect(),
        room: odds::MAX_BETS_PER_ROUND - staked,
        max_bet: odds::MAX_BET,
        net_label: if net > 0 { format!("+{net}") } else { net.to_string() },
        user: viewer.map(UserView::from),
    }))
}

#[derive(Deserialize)]
pub struct BetForm {
    pub market: String,
    pub stake: String,
    /// The odds the visitor was shown, in hundredths: the bet is placed at
    /// these or not at all.
    pub odds: i64,
    /// The round the page was for.
    pub round: String,
}

fn back(code: &str) -> Response {
    Redirect::to(&format!("/g/bets?msg={code}")).into_response()
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
        return Ok(back("unverified"));
    }
    let open = Round::current(state.now()).key();
    if form.round != open {
        return Ok(back("bet_closed"));
    }
    let Some(market) = Market::parse(&form.market) else {
        return Ok(back("bet_market"));
    };
    let Ok(stake) = form.stake.trim().parse::<i64>() else {
        return Ok(back("bet_cap"));
    };
    if !state.limits.take(&format!("bet:{}", viewer.account.id), limits::BETS_PER_ACCOUNT) {
        return Ok(back("bet_slow"));
    }
    let offer = todays_offers(&state).await?.into_iter().find(|o| o.market == market);
    let Some(offer) = offer.filter(|o| o.odds_x100 == form.odds) else {
        return Ok(back("bet_odds"));
    };
    crate::economy::collect(&state.db, viewer.account.id, &open).await?;
    let code = match db::place_bet(&state.db, viewer.account.id, &open, &market.key(), stake, offer.odds_x100).await? {
        Placed::In => "bet_placed",
        Placed::Already => "bet_twice",
        Placed::OverCap => "bet_cap",
        Placed::Broke => "broke",
        Placed::Closed => "bet_closed",
    };
    Ok(back(code))
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
        db::place_bet(db, who, round, market, stake, odds).await.unwrap()
    }

    #[tokio::test]
    async fn a_bet_takes_its_stake_once_within_the_caps() {
        let t = temp_db().await;
        let ann = player(&t.db, "ann").await;
        collect(&t.db, ann, "2026-10-21").await.unwrap();
        let start = db::balance(&t.db, ann).await.unwrap();

        assert_eq!(bet(&t.db, ann, "2026-10-21", "under:10", 50, 135).await, Placed::In);
        assert_eq!(db::balance(&t.db, ann).await.unwrap(), start - 50);
        assert_eq!(bet(&t.db, ann, "2026-10-21", "under:10", 10, 135).await, Placed::Already, "one per market");
        assert_eq!(bet(&t.db, ann, "2026-10-21", "over:10", 101, 260).await, Placed::OverCap, "per bet");
        assert_eq!(bet(&t.db, ann, "2026-10-21", "over:10", 0, 260).await, Placed::OverCap);
        assert_eq!(bet(&t.db, ann, "2026-10-21", "over:10", 100, 260).await, Placed::In, "both sides is allowed");
        assert_eq!(bet(&t.db, ann, "2026-10-21", "none", 60, 980).await, Placed::OverCap, "200 a round");
        assert_eq!(bet(&t.db, ann, "2026-10-21", "none", 50, 980).await, Placed::In);
        assert_eq!(db::balance(&t.db, ann).await.unwrap(), start - 200);
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
        assert_eq!(bet(&t.db, ann, "2026-10-21", "under:10", 50, 135).await, Placed::In);
        assert_eq!(bet(&t.db, ann, "2026-10-21", "over:10", 20, 260).await, Placed::In);
        assert_eq!(bet(&t.db, bob, "2026-10-21", "none", 10, 980).await, Placed::In);
        let (a0, b0) = (db::balance(&t.db, ann).await.unwrap(), db::balance(&t.db, bob).await.unwrap());

        settle_all(&t.db, "2026-10-22").await.unwrap();
        assert_eq!(db::balance(&t.db, ann).await.unwrap(), a0 + 67, "50 at ×1.35 pays 67; over loses");
        assert_eq!(db::balance(&t.db, bob).await.unwrap(), b0, "nobody-wins lost");
        let slips = db::bets_of(&t.db, ann, "").await.unwrap();
        let won = slips.iter().find(|b| b.market == "under:10").unwrap();
        assert_eq!((won.result.as_deref(), won.payout), (Some("won"), Some(67)));
        let lost = slips.iter().find(|b| b.market == "over:10").unwrap();
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
        bet(&t.db, bob, "2026-10-21", "under:5", 10, 180).await;
        let (a0, b0) = (db::balance(&t.db, ann).await.unwrap(), db::balance(&t.db, bob).await.unwrap());
        settle_all(&t.db, "2026-10-22").await.unwrap();
        assert_eq!(db::balance(&t.db, ann).await.unwrap(), a0 + 98);
        assert_eq!(db::balance(&t.db, bob).await.unwrap(), b0);
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
        bet(&t.db, ann, "2026-10-21", "under:5", 30, 180).await;
        bet(&t.db, cheat, "2026-10-21", "over:5", 30, 180).await;
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

    // ------------------------------------------------------------- pages

    async fn bets_page(state: &AppState, token: Option<&str>, msg: Option<&str>) -> String {
        let jar = token.map(jar).unwrap_or_default();
        let q = Query(BetsQuery { msg: msg.map(str::to_string) });
        body(page(State(state.clone()), jar, q).await.unwrap()).await
    }

    fn form(state: &AppState, market: &str, stake: &str, odds: i64) -> Form<BetForm> {
        Form(BetForm {
            market: market.into(),
            stake: stake.into(),
            odds,
            round: Round::current(state.now()).key(),
        })
    }

    async fn odds_for(state: &AppState, market: Market) -> i64 {
        todays_offers(state).await.unwrap().into_iter().find(|o| o.market == market).unwrap().odds_x100
    }

    #[tokio::test]
    async fn the_page_lists_every_market_and_takes_bets_from_players() {
        let (_t, state) = app(&[]).await;
        player(&state.db, "ann").await;
        let out = bets_page(&state, None, None).await;
        assert!(out.contains("winner under 10") && out.contains("nobody wins") && out.contains("winner 50 or more"));
        assert!(!out.contains(r#"action="/g/bet""#), "signed out: no forms");
        let html = bets_page(&state, Some("ann"), None).await;
        assert_eq!(html.matches(r#"action="/g/bet""#).count(), 9);
        assert!(html.contains("200 left to stake this round"));

        let o = odds_for(&state, Market::Under(10)).await;
        let resp = place(State(state.clone()), jar("ann"), form(&state, "under:10", "40", o)).await.unwrap();
        assert_eq!(location(&resp), "/g/bets?msg=bet_placed");
        let html = bets_page(&state, Some("ann"), Some("bet_placed")).await;
        assert!(html.contains("bet placed · settled at 12:42"));
        assert!(html.contains("your bets today"));
        assert!(html.contains("160 left to stake this round"));
        assert_eq!(html.matches(r#"action="/g/bet""#).count(), 8, "that market is taken");
        assert!(html.contains(&format!("<td class=\"num b\">{}</td>", odds::payout(40, o))));
    }

    #[tokio::test]
    async fn a_bet_is_placed_at_the_odds_shown_or_not_at_all() {
        let (_t, state) = app(&[]).await;
        let ann = player(&state.db, "ann").await;
        let o = odds_for(&state, Market::Nobody).await;
        for (f, code) in [
            (form(&state, "none", "10", o + 5), "bet_odds"),
            (form(&state, "under:7", "10", o), "bet_market"),
            (form(&state, "none", "lots", o), "bet_cap"),
            (Form(BetForm { market: "none".into(), stake: "10".into(), odds: o, round: "2000-01-01".into() }), "bet_closed"),
        ] {
            let resp = place(State(state.clone()), jar("ann"), f).await.unwrap();
            assert_eq!(location(&resp), format!("/g/bets?msg={code}"));
        }
        assert!(db::bets_of(&state.db, ann, "").await.unwrap().is_empty());
        // Not signed in: to sign in. Not confirmed: told so.
        let resp = place(State(state.clone()), CookieJar::new(), form(&state, "none", "10", o)).await.unwrap();
        assert_eq!(location(&resp), "/login");
        stranger(&state.db, "new", "new").await;
        let resp = place(State(state.clone()), jar("new"), form(&state, "none", "10", o)).await.unwrap();
        assert_eq!(location(&resp), "/g/bets?msg=unverified");
    }

    #[tokio::test]
    async fn odds_stay_put_for_the_day_and_a_placed_bet_keeps_its_own() {
        let (_t, state) = app(&[]).await;
        let ann = player(&state.db, "ann").await;
        let first = todays_offers(&state).await.unwrap();
        // Rounds closing in the past do not move today's odds once set...
        let bob = player(&state.db, "bob").await;
        let yesterday = Round::current(state.now()).previous_date().format("%Y-%m-%d").to_string();
        db::insert_guess(&state.db, Pool::Global, &yesterday, bob, 40, true).await.unwrap();
        assert_eq!(todays_offers(&state).await.unwrap(), first, "cached for the round");
        // ...and a bet carries the odds it was placed at, whatever comes later.
        let o = odds_for(&state, Market::Over(20)).await;
        place(State(state.clone()), jar("ann"), form(&state, "over:20", "10", o)).await.unwrap();
        *state.offers.lock().unwrap() = None;
        assert_eq!(db::bets_of(&state.db, ann, "").await.unwrap()[0].odds_x100, o);
    }

    #[tokio::test]
    async fn scripts_are_slowed_down() {
        let (_t, state) = app(&[]).await;
        player(&state.db, "ann").await;
        let o = odds_for(&state, Market::Nobody).await;
        let mut last = String::new();
        for _ in 0..31 {
            let resp = place(State(state.clone()), jar("ann"), form(&state, "none", "1", o)).await.unwrap();
            last = location(&resp).to_string();
        }
        assert_eq!(last, "/g/bets?msg=bet_slow");
    }

    #[tokio::test]
    async fn the_global_round_page_points_to_the_bets_and_the_42_round_does_not() {
        let (_t, state) = app(&[]).await;
        student(&state.db, 1001, "megardes", "s").await;
        let q = || Query(crate::handlers::IndexQuery { msg: None, n: None });
        let global = body(crate::handlers::global_index(State(state.clone()), jar("s"), q()).await.unwrap()).await;
        assert!(global.contains(r#"<a href="/g/bets">all bets &rarr;</a>"#));
        assert!(global.contains("under 10 ×"));
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
        bet(&state.db, ann, &day, "under:5", 10, 180).await;
        settle_all(&state.db, &Round::current(state.now()).key()).await.unwrap();
        let html = body(crate::points_pages::points(State(state.clone()), jar("ann")).await.unwrap()).await;
        assert!(html.contains(">bet<") && html.contains(">bet won<") && html.contains("+18"), "{html}");
        let page = bets_page(&state, Some("ann"), None).await;
        assert!(page.contains("past bets") && page.contains("net +8"));
    }
}
