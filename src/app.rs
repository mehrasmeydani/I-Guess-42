use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, TimeDelta, Utc};

use crate::config::Config;
use crate::db::Db;
use crate::limits::Limits;
use crate::mail::Mailer;
use crate::templates::{self, ErrorTemplate};

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub cfg: Arc<Config>,
    pub http: reqwest::Client,
    pub mailer: Mailer,
    /// Counts sign-in failures and sign-ups, per address and per email.
    pub limits: Arc<Limits>,
    /// Seconds added to the wall clock, so a test instance can be pushed past
    /// a 12:42 deadline without waiting for one. Always zero unless an admin
    /// moved it, and it only affects which round is open - never session
    /// expiry or the timestamps written to the database.
    clock_offset: Arc<AtomicI64>,
}

impl AppState {
    pub fn new(db: Db, cfg: Arc<Config>, http: reqwest::Client) -> Self {
        Self {
            mailer: Mailer::from_config(&cfg),
            limits: Arc::new(Limits::default()),
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
#[derive(Debug)]
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
        pool: crate::db::Pool::Vienna,
        user: None,
        test_mode: false,
        impersonating: false,
        status: status.as_u16(),
        message: message.to_string(),
    });
    (status, body).into_response()
}

/// The address a request came from: the right-most X-Forwarded-For entry
/// when TRUST_PROXY says a proxy (Caddy) put it there - the proxy appends
/// the address it saw, so anything to its left is the client's own claim -
/// and otherwise the TCP peer. `None` when neither is known, as in tests.
pub struct ClientIp(pub Option<std::net::IpAddr>);

impl axum::extract::FromRequestParts<AppState> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if state.cfg.trust_proxy {
            let forwarded = parts
                .headers
                .get_all("x-forwarded-for")
                .iter()
                .filter_map(|v| v.to_str().ok())
                .flat_map(|v| v.split(','))
                .rev()
                .find_map(|ip| ip.trim().parse().ok());
            if forwarded.is_some() {
                return Ok(ClientIp(forwarded));
            }
        }
        let peer = parts
            .extensions
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|c| c.0.ip());
        Ok(ClientIp(peer))
    }
}

#[cfg(test)]
mod tests {
    use axum::extract::{ConnectInfo, FromRequestParts};
    use axum::http::Request;

    use super::*;
    use crate::testkit::{app_with, config};

    async fn ip_of(trust_proxy: bool, forwarded: &[&str], peer: Option<&str>) -> Option<String> {
        let mut cfg = config(&[]);
        cfg.trust_proxy = trust_proxy;
        let (_t, state, _) = app_with(cfg).await;
        let mut req = Request::builder().uri("/");
        for f in forwarded {
            req = req.header("x-forwarded-for", *f);
        }
        let mut req = req.body(()).unwrap();
        if let Some(peer) = peer {
            req.extensions_mut().insert(ConnectInfo::<std::net::SocketAddr>(peer.parse().unwrap()));
        }
        let (mut parts, _) = req.into_parts();
        let ClientIp(ip) = ClientIp::from_request_parts(&mut parts, &state).await.unwrap();
        ip.map(|ip| ip.to_string())
    }

    #[tokio::test]
    async fn behind_the_proxy_the_address_it_appended_counts() {
        // A client claiming to be 1.1.1.1; Caddy appends what it really saw.
        let ip = ip_of(true, &["1.1.1.1, 203.0.113.7"], Some("172.18.0.3:5000")).await;
        assert_eq!(ip.as_deref(), Some("203.0.113.7"));
        // Across repeated headers too.
        let ip = ip_of(true, &["1.1.1.1", "203.0.113.7"], None).await;
        assert_eq!(ip.as_deref(), Some("203.0.113.7"));
        // Nothing usable forwarded: the peer.
        let ip = ip_of(true, &["garbage"], Some("172.18.0.3:5000")).await;
        assert_eq!(ip.as_deref(), Some("172.18.0.3"));
    }

    #[tokio::test]
    async fn without_a_trusted_proxy_the_header_is_ignored() {
        let ip = ip_of(false, &["1.1.1.1"], Some("198.51.100.4:6000")).await;
        assert_eq!(ip.as_deref(), Some("198.51.100.4"));
        assert_eq!(ip_of(false, &["1.1.1.1"], None).await, None);
    }
}
