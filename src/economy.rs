//! In-game points for the global round (SPEC.md section 8): fake, for
//! bragging only - never bought, sold, transferred or exchanged for anything.
//!
//! Points come in as a sign-up bonus and a daily grant for showing up, go
//! out as a stake on every guess, and the stakes of a round are its pot: the
//! winner takes the lot, and a round nobody won rolls its pot into the next.
//! Every movement is a row in `ledger`, which refuses to write the same
//! credit or charge twice, so nothing here needs to be careful about running
//! more than once.
//!
//! The 42 round has no points: 42 students there get coalition points from
//! 42 (points.rs). In the global round they earn these like everyone else.

use std::time::Duration;

use anyhow::Result;

use crate::app::AppState;
use crate::db::{self, Db};
use crate::round::Round;

/// Credited once, the first time a verified account visits the global round.
pub const SIGNUP_BONUS: i64 = 500;
/// Credited once per round to a verified account that visits the global
/// round. Visit to collect: an account nobody uses does not pile up points.
pub const DAILY_GRANT: i64 = 100;
/// What a guess costs. It goes into the round's pot.
pub const GUESS_STAKE: i64 = 10;

/// Every this many days in a row played in the global round...
pub const STREAK_DAYS: i64 = 10;
/// ...is worth this, once the player claims it.
pub const STREAK_BONUS: i64 = 1000;

/// How often the settler looks for closed rounds to pay out.
const SETTLE_EVERY: Duration = Duration::from_secs(30);

/// The sign-up bonus, if not had yet, and the open round's grant, if not
/// had yet. For a verified account only; call it whenever one turns up.
pub async fn collect(db: &Db, account_id: i64, open_round: &str) -> Result<()> {
    db::credit_once(db, account_id, open_round, "signup", SIGNUP_BONUS, "signup", None).await?;
    db::credit_once(db, account_id, open_round, "grant", DAILY_GRANT, open_round, None).await?;
    Ok(())
}

/// Streak bonuses earned and not claimed yet: the days that completed ten
/// in a row (today's guess counts), each as its ledger ref "streak:<day>".
pub async fn unclaimed_streaks(db: &Db, account_id: i64) -> Result<Vec<String>> {
    let days: Vec<chrono::NaiveDate> = db::global_play_days(db, account_id)
        .await?
        .iter()
        .filter_map(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .collect();
    let claimed = db::ledger_refs(db, account_id, "streak").await?;
    Ok(crate::stats::streak_milestones(&days, STREAK_DAYS)
        .into_iter()
        .map(|d| format!("streak:{}", d.format("%Y-%m-%d")))
        .filter(|r| !claimed.contains(r))
        .collect())
}

/// Credits every streak bonus earned and not claimed yet, each at most once
/// however often this runs. Returns the points credited.
pub async fn claim_streaks(db: &Db, account_id: i64) -> Result<i64> {
    let mut got = 0;
    for r in unclaimed_streaks(db, account_id).await? {
        let day = r.trim_start_matches("streak:").to_string();
        if db::credit_once(db, account_id, &day, "streak", STREAK_BONUS, &r, Some("10 days in a row")).await? {
            got += STREAK_BONUS;
        }
    }
    Ok(got)
}

/// Pays out every closed global round not settled yet, oldest first.
/// Returns how many it settled.
pub async fn settle_all(db: &Db, open_round: &str) -> Result<usize> {
    let mut settled = 0;
    for round in db::unsettled_rounds(db, open_round).await? {
        if let Some(pot) = db::settle_round(db, &round).await? {
            settled += 1;
            tracing::info!(
                round = %round,
                pot = pot.stakes + pot.carried_in,
                winner = ?pot.winner_id,
                players = pot.players,
                "settled the global round"
            );
        }
    }
    Ok(settled)
}

/// Runs for the life of the process. No external service is involved, so a
/// failure is a bug: logged loudly, and tried again on the next tick.
/// Follows the game's clock, so a test instance's admin can move it past a
/// deadline and watch the round settle.
pub async fn run(state: AppState) {
    let mut tick = tokio::time::interval(SETTLE_EVERY);
    loop {
        tick.tick().await;
        let open = Round::current(state.now()).key();
        if let Err(err) = settle_all(&state.db, &open).await {
            tracing::error!(error = ?err, "settling global rounds failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::extract::{Form, Query, State};
    use axum_extra::extract::cookie::CookieJar;

    use super::*;
    use crate::app::ClientIp;
    use crate::db::{test_support::temp_db, Locked, Pool};
    use crate::handlers::{self, GuessForm, IndexQuery};
    use crate::testkit::{app, body, jar, location, stranger, student};

    /// A verified account with a session named after it.
    async fn player(db: &Db, name: &str) -> i64 {
        stranger(db, name, name).await;
        sqlx::query("UPDATE accounts SET verified_at = 'x' WHERE username = ?1").bind(name).execute(db).await.unwrap();
        db::viewer_for_session(db, name).await.unwrap().unwrap().account.id
    }

    /// Enough points to play, and a guess locked for `round`.
    async fn plays(db: &Db, who: i64, round: &str, value: i64) {
        collect(db, who, round).await.unwrap();
        assert_eq!(db::lock_global_guess(db, round, who, value, GUESS_STAKE).await.unwrap(), Locked::In);
    }

    async fn rows(db: &Db, sql: &str) -> i64 {
        sqlx::query_as::<_, (i64,)>(sql).fetch_one(db).await.unwrap().0
    }

    #[tokio::test]
    async fn ten_days_in_a_row_is_a_bonus_claimed_once() {
        let (_t, state) = app(&[]).await;
        let ann = player(&state.db, "ann").await;
        let today = Round::current(state.now()).date;
        let day = |back: i64| (today - chrono::Duration::days(back)).format("%Y-%m-%d").to_string();
        // Nine days in a row, ending yesterday: nothing yet.
        for back in 1..=9 {
            plays(&state.db, ann, &day(back), 2).await;
        }
        assert!(unclaimed_streaks(&state.db, ann).await.unwrap().is_empty());
        let index = || handlers::global_index(State(state.clone()), jar("ann"), Query(IndexQuery { msg: None, n: None }));
        assert!(!body(index().await.unwrap()).await.contains("claim-card"));

        // Today makes ten: the pop-up offers 1 000.
        plays(&state.db, ann, &day(0), 2).await;
        assert_eq!(unclaimed_streaks(&state.db, ann).await.unwrap(), vec![format!("streak:{}", day(0))]);
        let html = body(index().await.unwrap()).await;
        assert!(html.contains("claim-card") && html.contains(&format!("+{}", crate::templates::points_label(&STREAK_BONUS))) && html.contains(r#"action="/g/streak""#), "{html}");

        let before = db::balance(&state.db, ann).await.unwrap();
        let resp = handlers::claim_streak(State(state.clone()), jar("ann")).await.unwrap();
        assert_eq!(location(&resp), "/g/?msg=streak_claimed");
        assert_eq!(db::balance(&state.db, ann).await.unwrap(), before + STREAK_BONUS);
        // Again: nothing more, and the pop-up is gone.
        let resp = handlers::claim_streak(State(state.clone()), jar("ann")).await.unwrap();
        assert_eq!(location(&resp), "/g/");
        assert_eq!(db::balance(&state.db, ann).await.unwrap(), before + STREAK_BONUS);
        assert!(!body(index().await.unwrap()).await.contains("claim-card"));
        let html = body(crate::points_pages::points(State(state.clone()), jar("ann")).await.unwrap()).await;
        assert!(html.contains("streak bonus"));
        // Signed out: to sign in.
        let resp = handlers::claim_streak(State(state.clone()), CookieJar::new()).await.unwrap();
        assert_eq!(location(&resp), "/login");
    }

    #[tokio::test]
    async fn the_welcome_and_daily_points_come_once_each() {
        let t = temp_db().await;
        let ann = player(&t.db, "ann").await;
        for _ in 0..3 {
            collect(&t.db, ann, "2026-10-21").await.unwrap();
        }
        assert_eq!(db::balance(&t.db, ann).await.unwrap(), SIGNUP_BONUS + DAILY_GRANT);
        // Many at once: still once.
        let tasks: Vec<_> = (0..20)
            .map(|_| {
                let db = t.db.clone();
                tokio::spawn(async move { collect(&db, ann, "2026-10-22").await.unwrap() })
            })
            .collect();
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(db::balance(&t.db, ann).await.unwrap(), SIGNUP_BONUS + 2 * DAILY_GRANT);
    }

    #[tokio::test]
    async fn a_guess_and_its_stake_go_in_together_or_not_at_all() {
        let t = temp_db().await;
        let ann = player(&t.db, "ann").await;
        // No points yet: refused, and nothing written.
        assert_eq!(db::lock_global_guess(&t.db, "2026-10-21", ann, 7, GUESS_STAKE).await.unwrap(), Locked::Broke);
        assert_eq!(rows(&t.db, "SELECT COUNT(*) FROM global_guesses").await, 0);
        assert_eq!(rows(&t.db, "SELECT COUNT(*) FROM ledger").await, 0);

        plays(&t.db, ann, "2026-10-21", 7).await;
        assert_eq!(db::balance(&t.db, ann).await.unwrap(), SIGNUP_BONUS + DAILY_GRANT - GUESS_STAKE);
        // Again with the same number: nothing more taken. Another number: refused.
        assert_eq!(db::lock_global_guess(&t.db, "2026-10-21", ann, 7, GUESS_STAKE).await.unwrap(), Locked::AlreadySame);
        assert_eq!(db::lock_global_guess(&t.db, "2026-10-21", ann, 8, GUESS_STAKE).await.unwrap(), Locked::AlreadyOther);
        assert_eq!(db::balance(&t.db, ann).await.unwrap(), SIGNUP_BONUS + DAILY_GRANT - GUESS_STAKE);
        assert_eq!(rows(&t.db, "SELECT COUNT(*) FROM ledger WHERE kind = 'stake'").await, 1);
    }

    #[tokio::test]
    async fn a_hundred_players_locking_at_once_all_get_in_and_pay_once() {
        // Each lock reads a balance and then writes a guess and a stake: the
        // pattern that failed with "database is locked" before write
        // transactions took the lock up front.
        let t = temp_db().await;
        let mut ids = vec![];
        for i in 0..100 {
            let id = player(&t.db, &format!("p{i}")).await;
            collect(&t.db, id, "2026-10-21").await.unwrap();
            ids.push(id);
        }
        let tasks: Vec<_> = ids
            .iter()
            .map(|&id| {
                let db = t.db.clone();
                tokio::spawn(async move { db::lock_global_guess(&db, "2026-10-21", id, 1, GUESS_STAKE).await.unwrap() })
            })
            .collect();
        for task in tasks {
            assert_eq!(task.await.unwrap(), Locked::In);
        }
        assert_eq!(db::pot_so_far(&t.db, "2026-10-21").await.unwrap(), (100 * GUESS_STAKE, 0));
        assert_eq!(rows(&t.db, "SELECT COUNT(*) FROM global_guesses").await, 100);
    }

    #[tokio::test]
    async fn the_winner_takes_every_stake() {
        let t = temp_db().await;
        let (ann, bob, cat) = (player(&t.db, "ann").await, player(&t.db, "bob").await, player(&t.db, "cat").await);
        for (who, v) in [(ann, 1), (bob, 1), (cat, 2)] {
            plays(&t.db, who, "2026-10-21", v).await;
        }
        let before = db::balance(&t.db, cat).await.unwrap();
        assert_eq!(settle_all(&t.db, "2026-10-22").await.unwrap(), 1);
        let pot = db::pot(&t.db, "2026-10-21").await.unwrap().unwrap();
        assert_eq!((pot.stakes, pot.carried_in, pot.winner_id, pot.paid, pot.players), (30, 0, Some(cat), 30, 3));
        assert_eq!(db::balance(&t.db, cat).await.unwrap(), before + 30);
        // Settling again pays nothing more.
        assert_eq!(settle_all(&t.db, "2026-10-22").await.unwrap(), 0);
        assert!(db::settle_round(&t.db, "2026-10-21").await.unwrap().is_none());
        assert_eq!(db::balance(&t.db, cat).await.unwrap(), before + 30);
        // Points are only moved, never made: stakes out, the pot back in.
        assert_eq!(
            rows(&t.db, "SELECT COALESCE(SUM(amount), 0) FROM ledger WHERE kind IN ('stake', 'pot')").await,
            0
        );
    }

    #[tokio::test]
    async fn an_unwon_pot_rolls_on_until_somebody_wins() {
        let t = temp_db().await;
        let (ann, bob, cat) = (player(&t.db, "ann").await, player(&t.db, "bob").await, player(&t.db, "cat").await);
        // Three days where everyone collides, then cat wins.
        for day in ["2026-10-18", "2026-10-19", "2026-10-20"] {
            plays(&t.db, ann, day, 5).await;
            plays(&t.db, bob, day, 5).await;
        }
        for (who, v) in [(ann, 1), (bob, 1), (cat, 4)] {
            plays(&t.db, who, "2026-10-21", v).await;
        }
        let before = db::balance(&t.db, cat).await.unwrap();
        assert_eq!(settle_all(&t.db, "2026-10-22").await.unwrap(), 4);
        let carried: Vec<(String, i64, i64)> =
            sqlx::query_as("SELECT round_date, carried_in, paid FROM pots ORDER BY round_date").fetch_all(&t.db).await.unwrap();
        assert_eq!(
            carried,
            [
                ("2026-10-18".into(), 0, 0),
                ("2026-10-19".into(), 20, 0),
                ("2026-10-20".into(), 40, 0),
                ("2026-10-21".into(), 60, 90),
            ]
        );
        assert_eq!(db::balance(&t.db, cat).await.unwrap(), before + 90);
        // And the next round starts from nothing.
        assert_eq!(db::pot_so_far(&t.db, "2026-10-22").await.unwrap(), (0, 0));
    }

    #[tokio::test]
    async fn the_open_pot_counts_what_rolled_in() {
        let t = temp_db().await;
        let (ann, bob) = (player(&t.db, "ann").await, player(&t.db, "bob").await);
        plays(&t.db, ann, "2026-10-20", 5).await;
        plays(&t.db, bob, "2026-10-20", 5).await;
        settle_all(&t.db, "2026-10-21").await.unwrap();
        plays(&t.db, ann, "2026-10-21", 3).await;
        assert_eq!(db::pot_so_far(&t.db, "2026-10-21").await.unwrap(), (10, 20));
    }

    #[tokio::test]
    async fn a_banned_players_stake_comes_back_and_their_guess_cannot_win() {
        let t = temp_db().await;
        let (ann, cheat, bob) = (player(&t.db, "ann").await, player(&t.db, "cheat").await, player(&t.db, "bob").await);
        plays(&t.db, ann, "2026-10-21", 5).await;
        plays(&t.db, cheat, "2026-10-21", 1).await;
        plays(&t.db, bob, "2026-10-21", 5).await;
        let before = db::balance(&t.db, cheat).await.unwrap();
        db::ban_account(&t.db, cheat, "alts", "2026-10-21").await.unwrap();
        assert_eq!(db::balance(&t.db, cheat).await.unwrap(), before + GUESS_STAKE);
        assert_eq!(db::pot_so_far(&t.db, "2026-10-21").await.unwrap(), (20, 0));
        settle_all(&t.db, "2026-10-22").await.unwrap();
        let pot = db::pot(&t.db, "2026-10-21").await.unwrap().unwrap();
        assert_eq!((pot.winner_id, pot.stakes, pot.players), (None, 20, 2), "with the cheat's 1 gone, everyone collided");
    }

    #[tokio::test]
    async fn a_called_off_round_refunds_everyone_and_takes_no_more_guesses() {
        let t = temp_db().await;
        let (ann, bob) = (player(&t.db, "ann").await, player(&t.db, "bob").await);
        plays(&t.db, ann, "2026-10-20", 5).await;
        plays(&t.db, bob, "2026-10-20", 5).await;
        settle_all(&t.db, "2026-10-21").await.unwrap(); // 20 rolls over
        plays(&t.db, ann, "2026-10-21", 1).await;
        let before = db::balance(&t.db, ann).await.unwrap();

        assert!(db::void_round(&t.db, "2026-10-21").await.unwrap());
        assert_eq!(db::balance(&t.db, ann).await.unwrap(), before + GUESS_STAKE);
        assert_eq!(db::lock_global_guess(&t.db, "2026-10-21", bob, 3, GUESS_STAKE).await.unwrap(), Locked::Voided);
        assert!(!db::void_round(&t.db, "2026-10-21").await.unwrap(), "once");
        let pot = db::pot(&t.db, "2026-10-21").await.unwrap().unwrap();
        assert!(pot.voided);
        assert_eq!(pot.paid, 0);
        // What rolled in rolls on past it.
        assert_eq!(db::pot_so_far(&t.db, "2026-10-22").await.unwrap(), (0, 20));
        // A settled round cannot be called off.
        assert!(!db::void_round(&t.db, "2026-10-20").await.unwrap());
    }

    #[tokio::test]
    async fn the_round_page_gives_points_and_shows_the_pot() {
        let (_t, state) = app(&[]).await;
        player(&state.db, "ann").await;
        let html = body(handlers::global_index(State(state.clone()), jar("ann"), Query(IndexQuery { msg: None, n: None })).await.unwrap()).await;
        let start = format!("{} pts", crate::templates::group_digits(SIGNUP_BONUS + DAILY_GRANT));
        assert!(html.contains(&format!(r#"href="/g/points" title="your points">{start}</a>"#)), "{html}");
        assert!(html.contains(r#"<p class="pot"><b class="win">0 pts</b><span>in the pot</span></p>"#));
        assert!(html.contains("lock it in &middot; 10 pts"));

        // Locking costs the stake, and the pot shows it.
        let round = Round::current(state.now()).key();
        let form = Form(GuessForm { guess: "7".into(), round: round.clone() });
        let resp = handlers::global_confirm_guess(State(state.clone()), ClientIp(None), jar("ann"), form).await.unwrap();
        assert_eq!(location(&resp), "/g?msg=saved");
        let html = body(handlers::global_index(State(state.clone()), jar("ann"), Query(IndexQuery { msg: None, n: None })).await.unwrap()).await;
        assert!(html.contains(r#"<p class="pot"><b class="win">10 pts</b><span>in the pot</span></p>"#));
        assert!(html.contains(&format!("{} pts</a>", SIGNUP_BONUS + DAILY_GRANT - GUESS_STAKE)));
    }

    #[tokio::test]
    async fn a_player_without_points_is_told_and_nothing_is_locked() {
        let (_t, state) = app(&[]).await;
        let ann = player(&state.db, "ann").await;
        let round = Round::current(state.now()).key();
        collect(&state.db, ann, &round).await.unwrap();
        db::credit_once(&state.db, ann, &round, "adjust", -(SIGNUP_BONUS + DAILY_GRANT), "x", Some("test")).await.unwrap();
        for resp in [
            handlers::global_submit_guess(State(state.clone()), jar("ann"), Form(GuessForm { guess: "7".into(), round: round.clone() })).await.unwrap(),
            handlers::global_confirm_guess(State(state.clone()), ClientIp(None), jar("ann"), Form(GuessForm { guess: "7".into(), round: round.clone() })).await.unwrap(),
        ] {
            assert_eq!(location(&resp), "/g?msg=broke");
        }
        assert_eq!(db::my_guess(&state.db, Pool::Global, &round, ann).await.unwrap(), None);
    }

    #[tokio::test]
    async fn the_42_round_has_no_points() {
        let (_t, state) = app(&[]).await;
        student(&state.db, 1001, "megardes", "s").await;
        let html = body(handlers::index(State(state.clone()), jar("s"), Query(IndexQuery { msg: None, n: None })).await.unwrap()).await;
        assert!(!html.contains(" pts"), "no points, no pot, no stake in the 42 round");
        let round = Round::current(state.now()).key();
        let form = Form(GuessForm { guess: "7".into(), round });
        handlers::confirm_guess(State(state.clone()), jar("s"), form).await.unwrap();
        assert_eq!(rows(&state.db, "SELECT COUNT(*) FROM ledger").await, 0);
    }

    #[tokio::test]
    async fn the_points_pages_show_the_history_and_the_board() {
        let (_t, state) = app(&[]).await;
        let (ann, bob) = (player(&state.db, "ann").await, player(&state.db, "bob").await);
        plays(&state.db, ann, "2026-10-20", 3).await;
        plays(&state.db, bob, "2026-10-20", 4).await;
        settle_all(&state.db, "2026-10-21").await.unwrap();

        let html = body(crate::points_pages::points(State(state.clone()), jar("ann")).await.unwrap()).await;
        for line in ["welcome points", "daily points", "your guess", "won the pot", "+500", "+100", "-10", "+20"] {
            assert!(html.contains(line), "{line}");
        }
        let board = |show: Option<&str>, token: Option<&str>| {
            let state = state.clone();
            let q = Query(crate::points_pages::BoardQuery { show: show.map(str::to_string) });
            let jar = token.map(jar).unwrap_or_default();
            async move { body(crate::points_pages::leaderboard(State(state), jar, q).await.unwrap()).await }
        };
        let html = board(None, Some("ann")).await;
        assert!(html.find(">ann<").unwrap() < html.find(">bob<").unwrap(), "ann won, so ann leads");
        // This week counts play only: ann +10 net, bob -10; grants left out.
        let html = board(Some("week"), Some("bob")).await;
        assert!(html.contains(">+10<") && html.contains(">-10<"), "{html}");
        assert!(!html.contains(">+610<"));
        // Signed out: labels, not names.
        let html = board(None, None).await;
        assert!(html.contains(">ann<"), "usernames are public");
        let resp = crate::points_pages::points(State(state.clone()), CookieJar::new()).await.unwrap();
        assert_eq!(location(&resp), "/login");
    }

    #[tokio::test]
    async fn the_day_page_says_where_the_pot_went() {
        let (_t, state) = app(&[]).await;
        let (ann, bob) = (player(&state.db, "ann").await, player(&state.db, "bob").await);
        let day = Round::current(state.now()).previous_date().format("%Y-%m-%d").to_string();
        plays(&state.db, ann, &day, 3).await;
        plays(&state.db, bob, &day, 3).await;
        settle_all(&state.db, &Round::current(state.now()).key()).await.unwrap();
        let html = body(handlers::global_day(State(state.clone()), jar("ann"), axum::extract::Path(day)).await.unwrap()).await;
        assert!(html.contains("no winner · pot 20 pts rolled over"));
    }

    #[tokio::test]
    async fn a_held_number_costs_its_stake_when_it_is_locked() {
        let (_t, state) = app(&[]).await;
        let hash = crate::credentials::hash_password("correct horse battery").await.unwrap();
        let db::NewAccount::Created(a) =
            db::create_password_account(&state.db, "ann@example.com", "ann", &hash, None).await.unwrap()
        else {
            panic!()
        };
        db::mark_email_verified(&state.db, a.id).await.unwrap();
        let round = Round::current(state.now()).key();
        let resp = handlers::global_submit_guess(State(state.clone()), CookieJar::new(), Form(GuessForm { guess: "7".into(), round })).await.unwrap();
        let token = crate::testkit::set_cookie(&resp, crate::held::COOKIE).unwrap();
        let resp = crate::signin::login_email(
            State(state.clone()),
            ClientIp(None),
            crate::testkit::sent(&[(crate::held::COOKIE, &token)]),
            Form(crate::signin::EmailLoginForm { login: "ann".into(), password: "correct horse battery".into() }),
        )
        .await
        .unwrap();
        assert_eq!(location(&resp), "/g?msg=saved");
        assert_eq!(db::balance(&state.db, a.id).await.unwrap(), SIGNUP_BONUS + DAILY_GRANT - GUESS_STAKE);
    }

    #[tokio::test]
    async fn a_moderator_can_adjust_and_call_off() {
        let mut cfg = crate::testkit::config(&[]);
        cfg.moderators = vec!["megardes".into()];
        let (_t, state, _) = crate::testkit::app_with(cfg).await;
        student(&state.db, 1001, "megardes", "mod").await;
        let ann = player(&state.db, "ann").await;
        let round = Round::current(state.now()).key();
        plays(&state.db, ann, &round, 4).await;
        let before = db::balance(&state.db, ann).await.unwrap();

        let adjust = |amount: &str, note: &str| {
            let state = state.clone();
            let f = crate::moderation::AdjustForm { username: "ann".into(), amount: amount.into(), note: note.into() };
            async move { crate::moderation::adjust(State(state), jar("mod"), Form(f)).await.unwrap() }
        };
        assert_eq!(adjust("50", "").await.status(), axum::http::StatusCode::UNPROCESSABLE_ENTITY, "a note is required");
        assert_eq!(adjust("fifty", "x").await.status(), axum::http::StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(location(&adjust("-50", "won with an alt").await), "/mod?msg=adjusted");
        assert_eq!(db::balance(&state.db, ann).await.unwrap(), before - 50);
        let history = body(crate::points_pages::points(State(state.clone()), jar("ann")).await.unwrap()).await;
        assert!(history.contains("adjusted by a moderator: won with an alt"));

        let void = |round: &str| {
            let state = state.clone();
            let f = crate::moderation::VoidForm { round: round.into() };
            async move { crate::moderation::void(State(state), jar("mod"), Form(f)).await.unwrap() }
        };
        assert_eq!(location(&void(&round).await), "/mod?msg=called_off");
        assert_eq!(db::balance(&state.db, ann).await.unwrap(), before - 50 + GUESS_STAKE);
        assert_eq!(void(&round).await.status(), axum::http::StatusCode::UNPROCESSABLE_ENTITY, "settled now");
        // Not a moderator: nothing there.
        let f = crate::moderation::VoidForm { round: round.clone() };
        let resp = crate::moderation::void(State(state.clone()), jar("ann"), Form(f)).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
    }
}
