use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, TimeDelta, Utc};
use tokio::sync::Mutex;

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
    pub login_limit: RequestLimit,
    pub history_limit: RequestLimit,
}

impl AppState {
    pub fn new(db: Db, cfg: Arc<Config>, http: reqwest::Client) -> Self {
        Self {
            db,
            cfg,
            http,
            clock_offset: Arc::new(AtomicI64::new(0)),
            login_limit: RequestLimit::new(60),
            history_limit: RequestLimit::new(120),
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
        impersonating: false,
        status: status.as_u16(),
        message: message.to_string(),
    });
    (status, body).into_response()
}

/// Instance-wide limits avoid trusting client-controlled forwarding headers.
#[derive(Clone)]
pub struct RequestLimit {
    requests: Arc<Mutex<VecDeque<Instant>>>,
    per_minute: usize,
}

impl RequestLimit {
    fn new(per_minute: usize) -> Self {
        Self {
            requests: Arc::new(Mutex::new(VecDeque::new())),
            per_minute,
        }
    }

    pub async fn allow(&self) -> bool {
        let mut requests = self.requests.lock().await;
        let now = Instant::now();
        while requests
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            requests.pop_front();
        }
        if requests.len() >= self.per_minute {
            return false;
        }
        requests.push_back(now);
        true
    }
}

pub fn too_many_requests() -> Response {
    (
        [(axum::http::header::RETRY_AFTER, "60")],
        error_page(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many requests. Try again in a minute.",
        ),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::RequestLimit;
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn request_limit_is_shared_bounded_and_recovers() {
        let limit = RequestLimit::new(2);
        let other = limit.clone();
        let (a, b, c) = tokio::join!(limit.allow(), other.allow(), limit.allow());
        assert_eq!([a, b, c].into_iter().filter(|allowed| *allowed).count(), 2);
        assert_eq!(limit.requests.lock().await.len(), 2);
        let expired = Instant::now() - Duration::from_secs(61);
        for request in limit.requests.lock().await.iter_mut() {
            *request = expired;
        }
        assert!(other.allow().await);
        assert_eq!(limit.requests.lock().await.len(), 1);
    }
}
