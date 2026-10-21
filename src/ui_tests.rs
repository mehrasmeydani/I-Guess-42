//! The UI/UX spec (requirements R1-R20) checked against rendered pages:
//! what each page says and links to, for whom, and the platform files.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Form;
use axum_extra::extract::cookie::CookieJar;

use crate::app::{AppState, ClientIp};
use crate::db::{self, Pool};
use crate::handlers::{self, IndexQuery, ResultsQuery, TrendsQuery};
use crate::round::Round;
use crate::testkit::{app, app_with, body, config, jar, location, stranger, student};

fn yesterday(state: &AppState) -> String {
    Round::current(state.now()).previous_date().format("%Y-%m-%d").to_string()
}

fn days_ago(state: &AppState, n: i64) -> String {
    (Round::current(state.now()).date - chrono::Duration::days(n)).format("%Y-%m-%d").to_string()
}

/// A verified account with a session, playing the global round.
async fn player(db: &db::Db, name: &str) -> i64 {
    stranger(db, name, name).await;
    sqlx::query("UPDATE accounts SET verified_at = 'x' WHERE username = ?1").bind(name).execute(db).await.unwrap();
    db::viewer_for_session(db, name).await.unwrap().unwrap().account.id
}

async fn guess(db: &db::Db, date: &str, who: i64, value: i64) {
    db::insert_guess(db, Pool::Global, date, who, value, true).await.unwrap();
}

/// Yesterday in the global round: 1 and 2 taken twice, 3 won by "cat",
/// "ann" picked 1. The day before, "ann" played too.
async fn a_played_day(state: &AppState) -> (i64, String) {
    let db = &state.db;
    let ann = player(db, "ann").await;
    let bob = player(db, "bob").await;
    let cat = player(db, "cat").await;
    let dan = player(db, "dan").await;
    let day = yesterday(state);
    for (who, v) in [(ann, 1), (bob, 1), (dan, 2), (cat, 3)] {
        guess(db, &day, who, v).await;
    }
    stranger(db, "eve", "eve-session").await;
    let eve = db::viewer_for_session(db, "eve-session").await.unwrap().unwrap().account.id;
    guess(db, &day, eve, 2).await;
    guess(db, &days_ago(state, 2), ann, 9).await;
    (ann, day)
}

async fn day_page(state: &AppState, token: Option<&str>, date: &str) -> String {
    let jar = token.map(jar).unwrap_or_default();
    body(handlers::global_day(State(state.clone()), jar, Path(date.to_string())).await.unwrap()).await
}

#[tokio::test]
async fn a_day_page_walks_the_calendar_and_leads_back_to_today() {
    let (_t, state) = app(&[]).await;
    let (_, day) = a_played_day(&state).await;
    let before = days_ago(&state, 2);
    let html = day_page(&state, Some("ann"), &day).await;
    // Linked by ISO date, shown the European way.
    let (eu_day, eu_before) = (crate::templates::eu(&day), crate::templates::eu(&before));
    assert!(html.contains(&format!(r#"href="/g/day/{before}" rel="prev">&larr; {eu_before}</a>"#)));
    assert!(!html.contains(r#"rel="next""#), "the newest closed round has no next");
    assert!(html.contains(r#"<a href="/g/">play today &rarr;</a>"#));
    let html = day_page(&state, Some("ann"), &before).await;
    assert!(html.contains(&format!(r#"href="/g/day/{day}" rel="next">{eu_day} &rarr;</a>"#)));
    assert!(!html.contains(r#"rel="prev""#), "the first round has no previous");
    assert!(html.contains(&format!("<title>round {eu_before} &middot; i guess low</title>")));
    assert!(!html.contains(&format!(">{before}<")), "no ISO date shown as text");
}

#[tokio::test]
async fn a_day_page_says_who_won_and_why_in_a_line_and_in_words() {
    let (_t, state) = app(&[]).await;
    let (_, day) = a_played_day(&state).await;
    let html = day_page(&state, Some("ann"), &day).await;
    assert!(html.contains("won · 5 players · 1 and 2 were all taken"), "the takeaway, not saying 3 twice");
    assert!(html.contains("5 players · 3 numbers · 2 shared numbers · most: 1 (2×) · winner: 3"));
    assert!(html.contains("winner: 3"));
    // Taken numbers struck through; the winner is not.
    assert!(html.contains("<s>1</s>") && html.contains("<s>2</s>"));
    assert!(!html.contains("<s>3</s>"));
    // A table alternative that says what it opens.
    assert!(html.contains("view as table &middot; 3 numbers"));
    assert!(html.contains("hide table"));
    assert!(html.contains("<caption class=\"sr\">every number picked in round"));
    assert!(!html.contains("historical") && !html.contains("demo data"), "only demo data is labelled; real rounds need no note");
    // Its own preview picture and description.
    assert!(html.contains(&format!(r#"content="http://localhost:3000/g/day/{day}/og.png""#)));
    assert!(html.contains(r#"<meta property="og:description" content="3 won · 5 players · 1 and 2 were all taken">"#));
}

#[tokio::test]
async fn your_own_pick_is_marked_and_shared_without_the_number() {
    let (_t, state) = app(&[]).await;
    let (_, day) = a_played_day(&state).await;
    let html = day_page(&state, Some("ann"), &day).await;
    assert!(html.contains("col many mine"), "ann's 1 is marked on the chart");
    assert!(html.contains("your pick: <b class=\"mine-text\">1</b>"));
    // The share text: outcome, winner, headcount, streak, link - not her 1.
    let start = html.find("data-share=\"").expect("a share button") + "data-share=\"".len();
    let text = &html[start..start + html[start..].find('"').unwrap()];
    assert!(text.contains("my pick: 🟥 (taken by 1)"), "{text}");
    assert!(text.contains("winner: 3 · 5 players"), "{text}");
    assert!(text.contains("🔥 2-day streak"), "{text}");
    assert!(text.contains(&format!("http://test/g/day/{day}")), "{text}");
    assert!(!text.lines().nth(1).unwrap().contains('1') || text.lines().nth(1).unwrap().contains("taken by 1"));
    // Someone who did not play that day gets no share button.
    assert!(!day_page(&state, None, &day).await.contains("data-share"));
}

#[test]
fn the_share_text_follows_the_design() {
    use crate::templates::{outcome_symbol, share_text};
    assert_eq!(outcome_symbol(3, 1, Some(3)), "🟩 won");
    assert_eq!(outcome_symbol(1, 4, Some(3)), "🟥 (taken by 3)");
    assert_eq!(outcome_symbol(9, 1, Some(3)), "🟨 unique, not lowest");
    let text = share_text("2026-10-21", "🟩 won", Some(3), 41, 1, "https://iguesslow.com/g/day/2026-10-21");
    assert_eq!(text, "i guess low · oct 21\nmy pick: 🟩 won\nwinner: 3 · 41 players\nhttps://iguesslow.com/g/day/2026-10-21");
    assert!(share_text("2026-10-21", "x", None, 2, 5, "u").contains("winner: nobody · 2 players\n🔥 5-day streak\n"));
}

#[tokio::test]
async fn a_rounds_preview_image_exists_only_once_it_has_closed() {
    let (_t, state) = app(&[]).await;
    let (_, day) = a_played_day(&state).await;
    let resp = handlers::global_day_image(State(state.clone()), Path(day.clone())).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "image/png");
    assert!(body_bytes(resp).await.starts_with(b"\x89PNG"));
    // The open round's numbers are secret.
    let today = Round::current(state.now()).key();
    player(&state.db, "zed").await;
    let resp = handlers::global_day_image(State(state.clone()), Path(today)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    // The 42 round's picture is for 42 students only.
    let resp = handlers::day_image(State(state.clone()), CookieJar::new(), Path(day)).await.unwrap();
    assert_eq!(location(&resp), "/static/og.png");
}

async fn body_bytes(resp: axum::response::Response) -> Vec<u8> {
    axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap().to_vec()
}

async fn results_page(state: &AppState, token: Option<&str>) -> String {
    let jar = token.map(jar).unwrap_or_default();
    let q = Query(ResultsQuery { q: None, show: None, date: None });
    body(handlers::global_results(State(state.clone()), jar, q).await.unwrap()).await
}

#[tokio::test]
async fn results_list_the_rounds_first_then_the_top_ten() {
    let (_t, state) = app(&[]).await;
    // Twelve days, each won by someone different.
    for i in 1..=12 {
        let who = player(&state.db, &format!("w{i}")).await;
        guess(&state.db, &days_ago(&state, i), who, 5).await;
    }
    let html = results_page(&state, Some("w1")).await;
    let rounds = html.find("recent rounds").unwrap();
    let leaders = html.find("most days won").unwrap();
    assert!(rounds < leaders, "rounds before the leaderboard");
    assert!(html.contains(&format!(r#"<td><a href="/g/day/{}">"#, yesterday(&state))));
    // Ten leaders shown, the rest behind a control that says what it does.
    let shown = &html[leaders..html[leaders..].find("<details").map_or(html.len(), |i| leaders + i)];
    assert_eq!(shown.matches("<tr title=").count(), 10);
    assert!(html.contains("show all 12"));
    assert!(html.contains("show top 10"));
    assert!(html.contains(r#"aria-current="true" href="/g/results">7</a>"#));
}

#[tokio::test]
async fn before_any_round_closes_results_look_ahead_and_filters_wait() {
    let (_t, state) = app(&[]).await;
    let html = results_page(&state, None).await;
    assert!(html.contains("no rounds yet &middot; first reveal"));
    assert!(html.contains(r#"id="q" name="q" type="search""#));
    assert_eq!(html.matches(" disabled>").count() + html.matches(" disabled ").count(), 4, "search, date and both buttons");
    let trends = body(
        handlers::global_trends(State(state.clone()), CookieJar::new(), Query(TrendsQuery { days: None, ..Default::default() }))
            .await
            .unwrap(),
    )
    .await;
    assert!(trends.contains("no data yet &middot; check back after 12:42"));
}

#[tokio::test]
async fn usernames_are_public_on_every_results_view() {
    let (_t, state) = app(&[]).await;
    let (_, day) = a_played_day(&state).await;
    // Signed out, as signed in: the username, never a stand-in label.
    for token in [None, Some("ann")] {
        let results = results_page(&state, token).await;
        assert!(results.contains("cat won with <b class=\"win\">3</b>"), "{token:?}");
        assert!(!results.contains("player #"));
        assert!(day_page(&state, token, &day).await.contains("cat won with"));
    }
    let board = body(
        crate::points_pages::leaderboard(State(state.clone()), CookieJar::new(), Query(Default::default())).await.unwrap(),
    )
    .await;
    assert!(!board.contains("player #"));
}

#[tokio::test]
async fn the_leaderboard_ranks_points_first_then_wins() {
    let (_t, state) = app(&[]).await;
    a_played_day(&state).await;
    // The results page opens with the last round analysed, as its day page does.
    let results = results_page(&state, None).await;
    assert!(results.contains(r#"<p class="why">1 and 2 were all taken</p>"#), "{results}");
    assert!(results.contains(r#"<p class="chart-summary">5 players · 3 numbers · 2 shared numbers · most: 1 (2×) · winner: 3</p>"#));
    crate::economy::collect(&state.db, db::viewer_for_session(&state.db, "ann").await.unwrap().unwrap().account.id, "2026-10-21").await.unwrap();
    let board = |pool: Pool, token: Option<&str>| {
        let state = state.clone();
        let jar = token.map(jar).unwrap_or_default();
        async move {
            let q = Query(Default::default());
            match pool {
                Pool::Global => crate::points_pages::leaderboard(State(state), jar, q).await.unwrap(),
                Pool::Vienna => crate::points_pages::vienna_leaderboard(State(state), jar, q).await.unwrap(),
            }
        }
    };
    let html = body(board(Pool::Global, None).await).await;
    let (points, wins) = (html.find(">most points").expect("points"), html.find(">most wins").expect("wins"));
    assert!(points < wins, "points first");
    assert!(html[wins..].contains(r#"<td class="who">cat</td>"#) && html[wins..].contains("<b>1</b>"), "cat won a day");
    assert!(html.contains(r#"href="/g/leaderboard">leaders</a>"#), "a tab in the header");
    assert!(html.contains(r#"class="on" aria-current="page" href="/g/leaderboard">leaders</a>"#));

    // Everyone with one win shares first place; nobody is "02" for a tie.
    let wins_part = &html[wins..];
    assert!(wins_part.contains(r#"<tr class="r1" title="cat"><td class="num place">01</td>"#), "{wins_part}");
    assert!(!wins_part.contains(">02<"));

    // The 42 round: wins only, and only for 42 students.
    student(&state.db, 1001, "megardes", "s").await;
    let html = body(board(Pool::Vienna, Some("s")).await).await;
    assert!(html.contains(">most wins") && !html.contains("most points"));
    let away = board(Pool::Vienna, None).await;
    assert_eq!(location(&away), "/g/leaderboard");
}

#[tokio::test]
async fn trends_open_with_the_ranges_and_one_analysing_line() {
    let (_t, state) = app(&["megardes"]).await;
    a_played_day(&state).await;
    let page = |days: &str| {
        let state = state.clone();
        let days = Some(days.to_string());
        async move {
            body(handlers::global_trends(State(state), CookieJar::new(), Query(TrendsQuery { days, ..Default::default() })).await.unwrap())
                .await
        }
    };
    let html = page("7").await;
    // Only the ranges up top: no scope line, no summary sentence.
    assert!(!html.contains("median winner") && !html.contains("demo data"), "{html}");
    assert!(html.contains(r#"href="/g/trends?days=custom">custom</a>"#));
    assert!(html.contains(r#"aria-current="true" href="/g/trends?days=7">7d</a>"#));
    // One line, terminal style: what was read, the bar, how long it took.
    let line = html.split("<p class=\"analyse\">").nth(1).expect("the analysing line").split("</p>").next().unwrap();
    assert!(line.starts_with("analysing 2 rounds, 6 picks "), "{line}");
    assert!(line.contains("[####################]") && line.contains("done in ") && line.contains(" ms"), "{line}");
    assert!(!html.contains("test details"));
    assert!(page("all").await.contains("what changed"));

    // A live site does not show it at all.
    let mut cfg = config(&[]);
    cfg.environment = Some("production".into());
    let (_t2, live, _) = app_with(cfg).await;
    let html = body(handlers::global_trends(State(live), CookieJar::new(), Query(TrendsQuery { days: None, ..Default::default() })).await.unwrap()).await;
    assert!(!html.contains("test details"));
}

#[tokio::test]
async fn my_rounds_show_the_record_and_every_round_played() {
    let (_t, state) = app(&[]).await;
    a_played_day(&state).await;
    let html = body(handlers::global_me(State(state.clone()), jar("ann")).await.unwrap()).await;
    assert!(html.contains("<dt>streak</dt><dd>2 <span class=\"dim\">days</span></dd>"), "{html}");
    assert!(html.contains("<dt>played</dt><dd>2</dd>"));
    // Alone two days ago, she won that one with 9.
    assert!(html.contains("<dt>won</dt><dd>1</dd>"));
    assert!(html.contains(r#"<dt>best win</dt><dd><span class="win">9</span></dd>"#));
    let first = html.find(&yesterday(&state)).unwrap();
    let second = html.find(&days_ago(&state, 2)).unwrap();
    assert!(first < second, "newest first");
    assert!(html.contains(r#"<span class="dim">lost</span>"#));
    assert!(html.contains(r#"<b class="win">won</b>"#));
    // Reached by the name in the header, not a tab; and on to the account.
    assert!(html.contains(r#"<a class="who" href="/g/me""#));
    assert!(!html.contains(r#">me</a>"#), "no me tab");
    assert!(html.contains(r#"href="/account">account &amp; log out"#));
    assert!(html.contains(r#"<form method="post" action="/logout"><button type="submit" class="out">log out</button></form>"#), "and log out in the header");
    // Yesterday's winner sees it as a win, and their best.
    let html = body(handlers::global_me(State(state.clone()), jar("cat")).await.unwrap()).await;
    assert!(html.contains(r#"<dt>best win</dt><dd><span class="win">3</span></dd>"#));
    // Signed out: to sign in. And no "me" tab.
    let resp = handlers::global_me(State(state.clone()), CookieJar::new()).await.unwrap();
    assert_eq!(location(&resp), "/login");
    let front = body(handlers::global_index(State(state.clone()), CookieJar::new(), Query(IndexQuery { msg: None, n: None })).await.unwrap()).await;
    assert!(!front.contains("/g/me\">me</a>"));
}

#[tokio::test]
async fn the_round_page_shows_a_players_streak() {
    let (_t, state) = app(&[]).await;
    a_played_day(&state).await;
    let html = body(handlers::global_index(State(state.clone()), jar("ann"), Query(IndexQuery { msg: None, n: None })).await.unwrap()).await;
    assert!(html.contains("&#128293; 2 &middot; played 2 &middot; won 1 &middot; best 9"), "{html}");
    assert!(html.contains("<title>today's round &middot; i guess low</title>"));
}

#[tokio::test]
async fn every_page_has_one_h1_and_the_sign_in_pages_do_not_repeat_it() {
    let (_t, state) = app(&[]).await;
    let login = body(crate::signin::login_page_handler(State(state.clone()), CookieJar::new(), Query(Default::default())).await.unwrap()).await;
    let signup = body(crate::signin::signup_page_handler(State(state.clone()), CookieJar::new()).await).await;
    let privacy = body(crate::moderation::privacy(State(state.clone()), CookieJar::new()).await.unwrap()).await;
    let front = body(handlers::global_index(State(state.clone()), CookieJar::new(), Query(IndexQuery { msg: None, n: None })).await.unwrap()).await;
    for (name, html) in [("login", &login), ("signup", &signup), ("privacy", &privacy), ("round", &front)] {
        assert_eq!(html.matches("<h1").count(), 1, "{name}");
    }
    assert!(login.contains(r#"<h1 class="auth-title">sign in</h1>"#));
    assert!(!login.contains(r#"<h2 class="auth-title">"#));
    // What each provider learns, beside its button; the 42 mark is decoration.
    assert!(login.contains("shares login, name, campus"));
    assert!(login.contains(r#"class="provider-mark mark-42" aria-hidden="true""#));
}

#[tokio::test]
async fn a_failed_sign_in_gets_a_focusable_summary_and_marked_fields() {
    let (_t, state) = app(&["megardes"]).await;
    let resp = crate::signin::login_email(
        State(state.clone()),
        ClientIp(None),
        CookieJar::new(),
        Form(crate::signin::EmailLoginForm { login: "nobody".into(), password: "wrong wrong".into() }),
    )
    .await
    .unwrap();
    let html = body(resp).await;
    assert!(html.contains(r#"class="error-summary" role="alert" tabindex="-1""#));
    assert!(html.contains(r#"<p class="field-error" id="login-error">wrong username, email or password</p>"#));
    assert_eq!(html.matches(r#"aria-invalid="true" aria-describedby="login-error""#).count(), 2);

    let resp = crate::signin::signup(
        State(state.clone()),
        ClientIp(None),
        Form(crate::signin::SignupForm {
            email: "ann@example.com".into(),
            username: "a".into(),
            password: "correct horse battery".into(),
            captcha: String::new(),
            nonce: String::new(),
            website: String::new(),
        }),
    )
    .await
    .unwrap();
    let html = body(resp).await;
    assert!(html.contains(r#"<p class="field-error" id="username-error">username: 3 to 20 characters</p>"#));
    assert!(html.contains(r#"aria-describedby="username-hint username-error""#));
    assert!(!html.contains("email-error"), "only the field that was wrong");
}

#[tokio::test]
async fn the_privacy_page_names_its_bases_processors_and_authority() {
    let (_t, state) = app(&[]).await;
    let html = body(crate::moderation::privacy(State(state), CookieJar::new()).await.unwrap()).await;
    for must in [
        "Art. 6(1)(b) GDPR", "Art. 6(1)(f)", "30 days", "24 hours", "1 hour", "2 days",
        "Google", "GitHub", "42", "Resend", "Data Privacy Framework",
        "Datenschutzbehörde", "ig42_session", "ig42_held",
    ] {
        assert!(html.contains(must), "{must}");
    }
    assert!(html.matches("<h2>").count() >= 4, "headed sections");
    assert!(!html.contains("Cloudflare"), "the robot check is our own: no third party to name");
}

#[tokio::test]
async fn the_platform_files_are_served() {
    let (_t, state) = app(&[]).await;
    a_played_day(&state).await;
    let manifest: serde_json::Value = serde_json::from_str(&body(crate::platform::manifest().await).await).unwrap();
    assert_eq!(manifest["start_url"], "/g/");
    assert_eq!(manifest["display"], "standalone");
    assert_eq!(manifest["theme_color"], "#0c0c0c");
    assert!(manifest["icons"].as_array().unwrap().len() >= 2);
    let robots = body(crate::platform::robots(State(state.clone())).await).await;
    assert!(robots.contains("Sitemap: http://test/sitemap.xml"));
    assert!(robots.contains("Disallow: /admin"));
    let sitemap = body(crate::platform::sitemap(State(state.clone())).await.unwrap()).await;
    assert!(sitemap.contains("<loc>http://test/g/</loc>"));
    assert!(sitemap.contains(&format!("<loc>http://test/g/day/{}</loc>", yesterday(&state))));
    assert!(!sitemap.contains("http://test/day/"), "the 42 round is not public");
    for file in ["static/favicon.ico", "static/apple-touch-icon.png", "static/og.png", "static/icon-192.png", "static/icon-512.png"] {
        assert!(std::path::Path::new(file).exists(), "{file}");
    }
}

#[tokio::test]
async fn pages_carry_an_etag_and_answer_304_when_unchanged() {
    use axum::routing::get;
    use tower::ServiceExt;
    let router = axum::Router::new()
        .route("/", get(|| async { axum::response::Html("<p>hello</p>") }))
        .layer(axum::middleware::from_fn(crate::platform::page_caching));
    let resp = router.clone().oneshot(axum::http::Request::get("/").body(axum::body::Body::empty()).unwrap()).await.unwrap();
    assert_eq!(resp.headers()["cache-control"], "private, no-cache");
    let etag = resp.headers()["etag"].clone();
    let again = router
        .oneshot(axum::http::Request::get("/").header("if-none-match", etag).body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn a_42_students_rounds_stay_in_the_42_round() {
    let (_t, state) = app(&[]).await;
    student(&state.db, 1001, "megardes", "s").await;
    let html = body(handlers::me(State(state.clone()), jar("s")).await.unwrap()).await;
    assert!(html.contains(r#"<a class="who" href="/me""#), "the name leads to the 42 round's record");
    let resp = handlers::me(State(state.clone()), jar("ann-none")).await.unwrap();
    assert_eq!(location(&resp), "/g/me", "anyone else is sent to the global one");
}

#[tokio::test]
async fn a_new_sso_account_picks_its_username_once_and_goes_on() {
    use crate::account::{welcome, welcome_save, WelcomeForm, WelcomeQuery};
    let (_t, state) = app(&[]).await;
    player(&state.db, "octo-cat").await;
    stranger(&state.db, "taken", "x").await;
    let page = body(
        welcome(State(state.clone()), jar("octo-cat"), Query(WelcomeQuery { then: Some("/g?msg=saved".into()) }))
            .await
            .unwrap(),
    )
    .await;
    assert!(page.contains(r#"value="octo-cat""#), "prefilled from the provider");
    assert!(page.contains(r#"name="then" value="/g?msg=saved""#));

    let save = |name: &str, then: &str| {
        let state = state.clone();
        let f = Form(WelcomeForm { username: name.into(), then: then.into() });
        async move { welcome_save(State(state), jar("octo-cat"), f).await.unwrap() }
    };
    assert_eq!(save("taken", "/g").await.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(save("x", "/g").await.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(location(&save("kai", "/g?msg=saved").await), "/g?msg=saved");
    let v = db::viewer_for_session(&state.db, "octo-cat").await.unwrap().unwrap();
    assert_eq!(v.account.username, "kai");
    // Picking here does not start the month-long wait to rename.
    assert_eq!(db::username_changed_at(&state.db, v.account.id).await.unwrap(), None);
    // Nowhere but the round: no open redirect.
    for evil in ["https://evil.example", "//evil.example", "/account/delete"] {
        assert_eq!(location(&save("kai", evil).await), "/g?msg=welcome", "{evil}");
    }
}

#[tokio::test]
async fn a_day_is_found_by_its_european_date() {
    let (_t, state) = app(&[]).await;
    let (_, day) = a_played_day(&state).await;
    let eu = crate::templates::eu(&day);
    let go = |date: &str| {
        let state = state.clone();
        let q = Query(ResultsQuery { q: None, show: None, date: Some(date.to_string()) });
        async move { handlers::global_results(State(state), CookieJar::new(), q).await.unwrap() }
    };
    assert_eq!(location(&go(&eu).await), format!("/g/day/{day}"));
    assert_eq!(location(&go(&day).await), format!("/g/day/{day}"), "ISO still works");
    assert_eq!(location(&go("31.02.2026").await), "", "not a date: no redirect");
    // And a search by European date or month finds it.
    let search = |q: &str| {
        let state = state.clone();
        let q = Query(ResultsQuery { q: Some(q.to_string()), show: None, date: None });
        async move { body(handlers::global_results(State(state), CookieJar::new(), q).await.unwrap()).await }
    };
    assert!(search(&eu).await.contains(&format!(r#"href="/g/day/{day}">{eu}</a>"#)));
    let month = &eu[3..];
    assert!(search(month).await.contains(&format!(r#"href="/g/day/{day}""#)));
}

async fn trends_with(state: &AppState, q: TrendsQuery) -> String {
    body(handlers::global_trends(State(state.clone()), CookieJar::new(), Query(q)).await.unwrap()).await
}

#[tokio::test]
async fn two_spans_of_your_choosing_are_compared() {
    let (_t, state) = app(&[]).await;
    let a = player(&state.db, "ann").await;
    let b = player(&state.db, "bob").await;
    // Span 1: three days with 2 players each; span 2: two days with 4.
    for n in [20, 19, 18] {
        let d = days_ago(&state, n);
        guess(&state.db, &d, a, 1).await;
        guess(&state.db, &d, b, 2).await;
    }
    let extra: Vec<i64> = futures_join(&state, 2).await;
    for n in [3, 2] {
        let d = days_ago(&state, n);
        for (i, who) in [a, b].into_iter().chain(extra.iter().copied()).enumerate() {
            guess(&state.db, &d, who, 10 + i as i64).await;
        }
    }
    // A day in neither span, which must not count.
    let lonely = player(&state.db, "zed").await;
    guess(&state.db, &days_ago(&state, 10), lonely, 1).await;

    let eu = |n| crate::templates::eu(&days_ago(&state, n));
    let html = trends_with(&state, TrendsQuery {
        days: Some("custom".into()),
        a_from: Some(eu(20)),
        a_to: Some(eu(18)),
        // Typed back to front: still the same span.
        b_from: Some(eu(2)),
        b_to: Some(eu(3)),
    })
    .await;
    assert!(html.contains("span 1<span class=\"when\">"), "{html}");
    assert!(html.contains("span 2<span class=\"when\">"));
    assert!(html.contains(&crate::templates::eu_span(&days_ago(&state, 20), &days_ago(&state, 18))));
    assert!(html.contains(&crate::templates::eu_span(&days_ago(&state, 3), &days_ago(&state, 2))));
    // Players per day: 2 against 4, so up by 2.
    let row = &html[html.find("players per day").unwrap()..];
    let row = &row[..row.find("</tr>").unwrap()];
    assert!(row.contains(">2</td>") && row.contains(">4</td>") && row.contains("+2"), "{row}");
    // The form keeps what was typed, European dates.
    assert!(html.contains(&format!(r#"name="a_from" type="text" inputmode="numeric" placeholder="01.10.2026" value="{}""#, eu(20))));
    // Five rounds analysed: the day in neither span is left out.
    assert!(html.contains(r#"<a href="/g/day/"#) && !html.contains(&format!(r#"href="/g/day/{}""#, days_ago(&state, 10))));
}

/// Two more players, verified, for a busier span.
async fn futures_join(state: &AppState, n: usize) -> Vec<i64> {
    let mut ids = vec![];
    for i in 0..n {
        ids.push(player(&state.db, &format!("extra{i}")).await);
    }
    ids
}

#[tokio::test]
async fn custom_with_nothing_typed_compares_last_months_first_week_with_the_last_week() {
    let (_t, state) = app(&[]).await;
    let html = trends_with(&state, TrendsQuery { days: Some("custom".into()), ..Default::default() }).await;
    use chrono::Datelike;
    let last = Round::current(state.now()).previous_date();
    let first_of_last = (last.with_day(1).unwrap() - chrono::Duration::days(1)).with_day(1).unwrap();
    assert!(html.contains(&format!(r#"value="{}""#, first_of_last.format("%d.%m.%Y"))), "{html}");
    assert!(html.contains(&format!(r#"value="{}""#, last.format("%d.%m.%Y"))));
    assert!(html.contains("no rounds in these spans"));
}

#[tokio::test]
async fn a_bad_custom_date_is_said_and_kept_for_fixing() {
    let (_t, state) = app(&[]).await;
    let html = trends_with(&state, TrendsQuery {
        days: Some("custom".into()),
        a_from: Some("yesterday".into()),
        ..Default::default()
    })
    .await;
    assert!(html.contains("dates look like 21.10.2026"));
    assert!(html.contains(r#"value="yesterday""#));
}

#[tokio::test]
async fn the_results_page_shows_the_last_rounds_chart() {
    let (_t, state) = app(&[]).await;
    a_played_day(&state).await;
    let html = results_page(&state, Some("ann")).await;
    assert!(html.contains(r#"<div class="chart""#));
    assert!(html.contains("col many mine"), "ann's own pick is marked");
}
