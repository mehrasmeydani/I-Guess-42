mod app;
mod auth;
mod config;
mod db;
mod handlers;
mod round;
mod templates;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::routing::{get, post};
use axum::Router;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use app::AppState;
use config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
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
        .nest_service("/static", ServeDir::new("static"))
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
