mod app;
mod auth;
mod config;
mod db;
mod demo;
mod handlers;
mod round;
mod stats;
mod templates;

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

    let db = db::connect(&cfg.database_url).await?;
    db::purge_expired(&db).await?;
    tokio::spawn(sweep_expired(db.clone()));

    let http = reqwest::Client::builder()
        .user_agent(concat!("i_guess_42/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(15))
        .build()
        .context("building the HTTP client")?;

    let cfg = Arc::new(cfg);
    if cfg.test_mode() {
        tracing::warn!(admins = ?cfg.admin_logins, "TEST MODE: /admin is reachable");
    }
    let state = AppState::new(db, cfg, http);

    let router = Router::new()
        .route("/", get(handlers::index))
        .route("/results", get(handlers::results))
        // Closed rounds only; the open round answers 404.
        .route("/day/{date}", get(handlers::day))
        .route("/trends", get(handlers::trends))
        .route("/guess", post(handlers::submit_guess))
        .route("/guess/confirm", post(handlers::confirm_guess))
        .route("/login", get(handlers::login))
        .route("/auth/callback", get(handlers::callback))
        .route("/logout", post(handlers::logout))
        .route("/healthz", get(handlers::healthz))
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
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("binding {bind_addr}"))?;
    tracing::info!("listening on http://{bind_addr}");

    axum::serve(listener, router)
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
