use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::config::Config;
use crate::db::Db;
use crate::templates::{self, ErrorTemplate};

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub cfg: Arc<Config>,
    pub http: reqwest::Client,
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
        status: status.as_u16(),
        message: message.to_string(),
    });
    (status, body).into_response()
}
