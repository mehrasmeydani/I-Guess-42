use askama::Template;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};

use crate::db::{LeaderboardRow, RoundSummary, User};
use crate::round;

/// Insert thin separators every three digits, so a 19-digit guess is readable.
pub fn group_digits(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push('\u{202f}'); // narrow no-break space
        }
        out.push(ch);
    }
    out
}

pub struct UserView {
    pub login: String,
    pub display_name: String,
    pub image_url: Option<String>,
}

impl From<User> for UserView {
    fn from(u: User) -> Self {
        Self {
            login: u.login,
            display_name: u.display_name,
            image_url: u.image_url,
        }
    }
}

pub struct WinnerView {
    pub login: String,
    pub display_name: String,
    pub image_url: Option<String>,
    pub value_label: String,
}

pub struct RoundView {
    pub date_label: String,
    pub total: i64,
    pub winner: Option<WinnerView>,
}

impl From<RoundSummary> for RoundView {
    fn from(r: RoundSummary) -> Self {
        // The winner columns are filled in together or not at all: a round
        // only has a winner if some value was picked exactly once.
        let winner = match (r.winner_login, r.winner_name, r.winning_value) {
            (Some(login), Some(display_name), Some(value)) => Some(WinnerView {
                login,
                display_name,
                image_url: r.winner_image,
                value_label: group_digits(value),
            }),
            _ => None,
        };
        Self {
            date_label: round::format_round_date(&r.round_date),
            total: r.total,
            winner,
        }
    }
}

pub struct LeaderView {
    pub login: String,
    pub display_name: String,
    pub image_url: Option<String>,
    pub wins: i64,
}

impl From<LeaderboardRow> for LeaderView {
    fn from(r: LeaderboardRow) -> Self {
        Self {
            login: r.login,
            display_name: r.display_name,
            image_url: r.image_url,
            wins: r.wins,
        }
    }
}

/// A one-shot banner above the guess form.
pub struct Notice {
    pub kind: &'static str, // "ok" or "error"
    pub text: String,
}

impl Notice {
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            kind: "ok",
            text: text.into(),
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            kind: "error",
            text: text.into(),
        }
    }
}

#[derive(Template)]
#[template(path = "index.html")]
pub struct IndexTemplate {
    pub user: Option<UserView>,
    /// `YYYY-MM-DD`, round-tripped through the form's hidden field.
    pub round_key: String,
    pub round_label: String,
    pub deadline_human: String,
    pub seconds_left: i64,
    /// Pre-formatted, so the page is right before the countdown script runs.
    pub time_left: String,
    pub my_guess_label: Option<String>,
    pub guess_count: i64,
    pub last_round: Option<RoundView>,
    pub notice: Option<Notice>,
}

#[derive(Template)]
#[template(path = "results.html")]
pub struct ResultsTemplate {
    pub user: Option<UserView>,
    pub rounds: Vec<RoundView>,
    pub leaders: Vec<LeaderView>,
}

#[derive(Template)]
#[template(path = "error.html")]
pub struct ErrorTemplate {
    pub user: Option<UserView>,
    pub status: u16,
    pub message: String,
}

/// Render a template, or fall back to a bare 500 if the template itself is broken.
pub fn render<T: Template>(template: &T) -> Response {
    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(err) => {
            tracing::error!(%err, "template rendering failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "template rendering failed",
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::group_digits;

    #[test]
    fn groups_digits_in_threes() {
        assert_eq!(group_digits(7), "7");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1_000), "1\u{202f}000");
        assert_eq!(group_digits(1_234_567), "1\u{202f}234\u{202f}567");
        assert_eq!(
            group_digits(i64::MAX),
            "9\u{202f}223\u{202f}372\u{202f}036\u{202f}854\u{202f}775\u{202f}807"
        );
    }
}
