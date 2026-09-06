use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, TimeDelta, Utc};

use crate::config::Config;
use crate::db::Db;
use crate::templates::{self, ErrorTemplate};

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub cfg: Arc<Config>,
    pub http: reqwest::Client,
    /// Seconds added to the wall clock, so a test instance can be pushed past
    /// a 12:42 deadline without waiting for one. Always zero unless an admin
    /// moved it, and it only affects which round is open - never session
    /// expiry or the timestamps written to the database.
    clock_offset: Arc<AtomicI64>,
}

impl AppState {
    pub fn new(db: Db, cfg: Arc<Config>, http: reqwest::Client) -> Self {
        Self {
            db,
            cfg,
            http,
            clock_offset: Arc::new(AtomicI64::new(0)),
        }
    }

    /// The current time as the *game* sees it.
    pub fn now(&self) -> DateTime<Utc> {
        Utc::now() + TimeDelta::seconds(self.clock_offset())
    }

    pub fn clock_offset(&self) -> i64 {
        self.clock_offset.load(Ordering::Relaxed)
    }

    pub fn shift_clock(&self, seconds: i64) {
        self.clock_offset.fetch_add(seconds, Ordering::Relaxed);
    }

    pub fn reset_clock(&self) {
        self.clock_offset.store(0, Ordering::Relaxed);
    }
}

/// Any error that escapes a handler. Logged in full, shown to the visitor as
/// a generic page so internal detail never reaches the browser.
pub struct AppError(anyhow::Error);

impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        tracing::error!(error = ?self.0, "request failed");
        error_page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Something broke on our side. Try again in a moment.",
        )
    }
}

pub fn error_page(status: StatusCode, message: &str) -> Response {
    let body = templates::render(&ErrorTemplate {
        user: None,
        test_mode: false,
        status: status.as_u16(),
        message: message.to_string(),
    });
    (status, body).into_response()
}
