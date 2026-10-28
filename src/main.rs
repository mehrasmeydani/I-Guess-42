mod account;
mod app;
mod bets;
mod auth;
mod config;
mod copy;
mod credentials;
mod db;
mod demo;
mod economy;
mod handlers;
mod held;
mod human;
mod ics;
mod limits;
mod mail;
mod moderation;
mod oauth;
mod odds;
mod board_png;
mod og;
mod platform;
mod points;
mod points_pages;
mod round;
mod signin;
mod stats;
mod templates;
#[cfg(test)]
mod testkit;
#[cfg(test)]
mod ui_tests;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::routing::{get, post};
use axum::Router;
use axum::http::{header, HeaderValue};
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeader;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use app::AppState;
use config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    // ENV_FILE picks a different file, e.g. `ENV_FILE=.env.test cargo run` for
    // a local test instance next to the live settings in .env. A named file
    // that is missing is an error; a missing default .env is fine (compose
    // passes the settings in as real environment variables).
    match std::env::var("ENV_FILE") {
        Ok(path) => {
            dotenvy::from_filename(&path).with_context(|| format!("loading ENV_FILE={path}"))?;
        }
        Err(_) => {
            dotenvy::dotenv().ok();
        }
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("i_guess_42=info,tower_http=warn")),
        )
        .init();

    let cfg = Config::from_env()?;
    let bind_addr = cfg.bind_addr.clone();
    let redirect_uri = cfg.redirect_uri.clone();

    let db = db::connect(&cfg.database_url).await?;
    db::purge_expired(&db).await?;
    tokio::spawn(sweep_expired(db.clone()));

    let http = reqwest::Client::builder()
        .user_agent(concat!("i_guess_42/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(15))
        .build()
        .context("building the HTTP client")?;

    templates::init_site(templates::Site {
        public_url: cfg.public_url.clone(),
        anon_key: cfg.anon_key.clone(),
    });
    let cfg = Arc::new(cfg);
    if cfg.test_mode() {
        tracing::warn!(admins = ?cfg.admin_logins, "TEST MODE: /admin is reachable");
    }
    match (&cfg.points_api_key, cfg.test_mode()) {
        (Some(key), false) => {
            let api = points::Api {
                url: cfg.points_api_url.clone(),
                key: key.clone(),
            };
            tokio::spawn(points::run(db.clone(), http.clone(), api));
        }
        (Some(_), true) => {
            tracing::warn!("IGLCP_API_KEY is ignored on a test instance: no coalition points")
        }
        (None, _) => {}
    }
    let state = AppState::new(db, cfg, http);
    // In-game points: fake ones, so they run on test instances too.
    tokio::spawn(economy::run(state.clone()));

    let router = Router::new()
        .route("/", get(handlers::index))
        .route("/results", get(handlers::results))
        // Closed rounds only; the open round answers 404.
        .route("/day/{date}", get(handlers::day))
        .route("/trends", get(handlers::trends))
        .route("/guess", post(handlers::submit_guess))
        .route("/guess/confirm", post(handlers::confirm_guess))
        // The global pool: the same pages over its own guesses.
        .route("/g", get(handlers::global_index))
        .route("/g/", get(handlers::global_index))
        .route("/g/results", get(handlers::global_results))
        .route("/g/day/{date}", get(handlers::global_day))
        .route("/g/day/{date}/og.png", get(handlers::global_day_image))
        .route("/day/{date}/og.png", get(handlers::day_image))
        .route("/g/trends", get(handlers::global_trends))
        .route("/g/me", get(handlers::global_me))
        .route("/g/points", get(points_pages::points))
        .route("/g/bets", get(bets::page))
        .route("/g/bets/quote", get(bets::quote))
        .route("/g/bet", post(bets::place))
        .route("/g/leaderboard", get(points_pages::leaderboard))
        .route("/leaderboard", get(points_pages::vienna_leaderboard))
        .route("/leaderboard.png", get(points_pages::vienna_board_image))
        .route("/me", get(handlers::me))
        .route("/g/guess", post(handlers::global_submit_guess))
        .route("/g/streak", post(handlers::claim_streak))
        .route("/g/guess/confirm", post(handlers::global_confirm_guess))
        .route("/login", get(signin::login_page_handler))
        .route("/login/42", get(oauth::login_42))
        .route("/login/{provider}", get(oauth::login))
        .route("/link/{provider}", get(oauth::link))
        .route("/auth/github/callback", get(oauth::github_callback))
        .route("/auth/google/callback", get(oauth::google_callback))
        .route("/login/email", post(signin::login_email))
        .route("/signup", get(signin::signup_page_handler).post(signin::signup))
        .route("/verify", get(signin::verify_page).post(signin::verify))
        .route("/verify/resend", post(signin::resend_verification))
        .route("/forgot", get(signin::forgot_page).post(signin::forgot))
        .route("/reset", get(signin::reset_page_handler).post(signin::reset))
        .route("/auth/callback", get(oauth::callback_42))
        .route("/logout", post(handlers::logout))
        .route("/account", get(account::account))
        .route("/welcome", get(account::welcome).post(account::welcome_save))
        .route("/account/username", post(account::rename))
        .route("/account/unlink", post(account::unlink))
        .route("/account/password", post(account::password))
        .route("/account/delete", post(account::delete))
        // 404 unless MODERATORS names the signed-in 42 login.
        .route("/mod", get(moderation::moderation))
        .route("/mod/ban", post(moderation::ban))
        .route("/mod/warn", post(moderation::warn))
        .route("/mod/unban", post(moderation::unban))
        .route("/mod/adjust", post(moderation::adjust))
        .route("/mod/rename", post(moderation::rename))
        .route("/mod/void", post(moderation::void))
        .route("/mod/points", get(moderation::points))
        .route("/privacy", get(moderation::privacy))
        .route("/imprint", get(moderation::imprint))
        .route("/healthz", get(handlers::healthz))
        .route("/api/round", get(handlers::round_now))
        .route("/manifest.webmanifest", get(platform::manifest))
        .route("/robots.txt", get(platform::robots))
        .route("/sitemap.xml", get(platform::sitemap))
        .route_service("/favicon.ico", tower_http::services::ServeFile::new("static/favicon.ico"))
        .route("/round/{file}", get(handlers::calendar))
        .route("/g/round/{file}", get(handlers::global_calendar))
        // 404 unless ADMIN_LOGINS names the signed-in user.
        .route("/admin", get(handlers::admin))
        .route("/admin/clock", post(handlers::admin_clock))
        .route("/admin/guess", post(handlers::admin_fake_guess))
        .route("/admin/clear", post(handlers::admin_clear))
        .route("/admin/demo", post(handlers::admin_demo))
        .route("/admin/demo/remove", post(handlers::admin_demo_remove))
        .route("/admin/impersonate", post(handlers::admin_impersonate))
        // Not admin-gated: the caller is a demo account by the time they need it.
        .route("/admin/return", post(handlers::admin_return))
        // no-cache: browsers keep the files but check back each time (a cheap
        // 304 when nothing changed), so a new stylesheet reaches everyone on
        // their next page load instead of whenever their cache expires.
        .nest_service(
            "/static",
            SetResponseHeader::overriding(
                ServeDir::new("static"),
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-cache"),
            ),
        )
        .fallback(handlers::not_found)
        .layer(axum::middleware::from_fn_with_state(state.clone(), app::error_pages_know_the_viewer))
        .layer(axum::middleware::from_fn(platform::page_caching))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("binding {bind_addr}"))?;
    tracing::info!("listening on http://{bind_addr}");
    // The 42 login always comes back to the host in FT_REDIRECT_URI. Browsing
    // on another name for the same machine (127.0.0.1 vs localhost) keeps its
    // own cookies and zoom, so after logging in you would look logged out.
    if let Ok(url) = reqwest::Url::parse(&redirect_uri) {
        if let Some(host) = url.host_str() {
            let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
            tracing::info!("open {}://{host}{port} in the browser", url.scheme());
        }
    }

    // With the peer address attached, for ClientIp when no proxy says otherwise.
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
        .context("running the server")?;
    Ok(())
}

/// Hourly cleanup of dead sessions and unused OAuth states.
async fn sweep_expired(db: db::Db) {
    let mut ticker = tokio::time::interval(Duration::from_secs(60 * 60));
    loop {
        ticker.tick().await;
        if let Err(err) = db::purge_expired(&db).await {
            tracing::warn!(%err, "sweeping expired rows failed");
        }
    }
}
