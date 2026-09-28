//! Coalition points for everyone who played a round, and a bigger share for
//! its winner, through the 42 Vienna points API
//! (https://iglcp-api.42vienna.com/docs).
//!
//! Shortly after every 12:42 deadline, and once at startup, the round that
//! just closed is settled: the winner's intra id and every participant's go to
//! the API in one request, and the answer is recorded in `payouts` so the same
//! round is never paid twice. The API itself refuses a second payout on the
//! same day (429), which covers the gap between a request succeeding and the
//! row being written.
//!
//! How much each id is worth is the API's business, not ours: we name who
//! played and who won, it pays [`PARTICIPANT_POINTS`] and [`WINNER_POINTS`].
//! Those two constants exist only so the front page can say the amounts out
//! loud, and they have to be kept in step with the API by hand.
//!
//! A round nobody won pays nobody: `winner_user_id` is required, so there is
//! no request to make for a day where every number collided.
//!
//! Off unless IGLCP_API_KEY is set, and never on a test instance, whose rounds
//! are full of invented players and a clock an admin can move.

use std::time::Duration;

use anyhow::{bail, Result};
use chrono::Utc;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::db::{self, Db, Payout};
use crate::round::Round;

pub const DEFAULT_API_URL: &str = "https://iglcp-api.42vienna.com";

/// What the API pays each player who took part in a round...
pub const PARTICIPANT_POINTS: i64 = 5;
/// ...and what it pays the one who won it.
pub const WINNER_POINTS: i64 = 100;

/// Wait this long past 12:42 before settling, so the last guesses have landed.
const AFTER_DEADLINE: Duration = Duration::from_secs(30);
/// Retries back off 30s, 1m, 2m, 4m, 8m: about a quarter of an hour in all.
const ATTEMPTS: u32 = 6;
const FIRST_RETRY: Duration = Duration::from_secs(30);

/// Where the API lives and the key it wants in the Authorization header.
#[derive(Debug, Clone)]
pub struct Api {
    pub url: String,
    pub key: String,
}

/// The request body the spec asks for: who won, and everyone who played.
#[derive(Serialize)]
struct GivePoints<'a> {
    winner_user_id: i64,
    participant_user_ids: &'a [i64],
}

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Given,
    /// 429: points already went out today, by us or by hand.
    AlreadyGiven,
    /// The API or intra behind it is down or unreachable; worth another try.
    Retry,
    /// A bad key or a bad request; sending it again changes nothing.
    Failed,
}

impl Outcome {
    /// The `payouts.outcome` value for a settled round.
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Given => "given",
            Outcome::AlreadyGiven => "already",
            Outcome::Retry => "retry",
            Outcome::Failed => "failed",
        }
    }
}

/// Reads an API answer. `reply` is `None` when the body was not the JSON the
/// spec promises, which happens when a proxy in front of it answers instead.
fn classify(status: StatusCode, reply: Option<Reply>) -> (Outcome, String) {
    let success = reply.as_ref().is_some_and(|r| r.success);
    let message = reply.and_then(|r| r.message).unwrap_or_default();
    let detail = format!("{status} {message}").trim_end().to_string();
    let outcome = match status {
        StatusCode::OK if success => Outcome::Given,
        StatusCode::TOO_MANY_REQUESTS => Outcome::AlreadyGiven,
        s if s.is_server_error() => Outcome::Retry,
        _ => Outcome::Failed,
    };
    (outcome, detail)
}

async fn give_points(http: &reqwest::Client, api: &Api, payout: &Payout) -> (Outcome, String) {
    let url = format!("{}/api/give_points", api.url.trim_end_matches('/'));
    let sent = http
        .post(url)
        .header(AUTHORIZATION, &api.key)
        // Exactly the type the spec names; .json() keeps a type already set.
        .header(CONTENT_TYPE, "application/json; charset=utf-8")
        .json(&GivePoints {
            winner_user_id: payout.winner_id,
            participant_user_ids: &payout.participant_ids,
        })
        .send()
        .await;
    match sent {
        Ok(resp) => {
            let status = resp.status();
            classify(status, resp.json::<Reply>().await.ok())
        }
        // Timeouts and refused connections: the request may never have
        // arrived. Trying again is safe, since a repeat only gets a 429.
        Err(err) => (Outcome::Retry, err.to_string()),
    }
}

/// Pays out `round_date` if it has an unpaid winner.
async fn settle(db: &Db, http: &reqwest::Client, api: &Api, open_round: &str, round_date: &str) -> Result<()> {
    let Some(payout) = db::unpaid_round(db, open_round, round_date).await? else {
        return Ok(());
    };
    let mut delay = FIRST_RETRY;
    for attempt in 1..=ATTEMPTS {
        let (outcome, detail) = give_points(http, api, &payout).await;
        match outcome {
            Outcome::Given | Outcome::AlreadyGiven => {
                db::record_payout(db, round_date, &payout, outcome.as_str(), &detail).await?;
                tracing::info!(
                    round = round_date,
                    winner = payout.winner_id,
                    players = payout.participant_ids.len(),
                    outcome = outcome.as_str(),
                    %detail,
                    "coalition points settled"
                );
                return Ok(());
            }
            Outcome::Failed => bail!("the points API refused: {detail}"),
            Outcome::Retry if attempt < ATTEMPTS => {
                tracing::warn!(round = round_date, attempt, %detail, "points API failed, retrying in {delay:?}");
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
            Outcome::Retry => bail!("the points API still failed after {ATTEMPTS} attempts: {detail}"),
        }
    }
    unreachable!("the last attempt always returns")
}

/// Runs for the life of the server. Uses the real clock on purpose: the admin
/// clock only exists on test instances, which never get here.
pub async fn run(db: Db, http: reqwest::Client, api: Api) {
    tracing::info!(
        url = %api.url,
        participant = PARTICIPANT_POINTS,
        winner = WINNER_POINTS,
        "coalition points on for every player and each round's winner"
    );
    loop {
        let open = Round::current(Utc::now());
        let closed = open.previous_date().format("%Y-%m-%d").to_string();
        if let Err(err) = settle(&db, &http, &api, &open.key(), &closed).await {
            // Left unsettled, so a restart tries again. The API pays out once
            // a day, so a round is only ever retried until the next one closes.
            tracing::error!(round = %closed, error = ?err, "coalition points were not paid");
        }
        let now = Utc::now();
        let left = Round::current(now).seconds_left(now).max(0) as u64;
        tokio::time::sleep(Duration::from_secs(left) + AFTER_DEADLINE).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(success: bool, message: &str) -> Option<Reply> {
        Some(Reply {
            success,
            message: Some(message.to_string()),
        })
    }

    #[test]
    fn a_successful_answer_is_a_payout() {
        let (o, detail) = classify(StatusCode::OK, reply(true, "done"));
        assert_eq!(o, Outcome::Given);
        assert_eq!(detail, "200 OK done");
    }

    #[test]
    fn ok_without_success_is_not_a_payout() {
        assert_eq!(classify(StatusCode::OK, reply(false, "")).0, Outcome::Failed);
        assert_eq!(classify(StatusCode::OK, None).0, Outcome::Failed);
    }

    #[test]
    fn already_paid_today_settles_the_round() {
        let (o, _) = classify(StatusCode::TOO_MANY_REQUESTS, reply(false, "already"));
        assert_eq!(o, Outcome::AlreadyGiven);
    }

    #[test]
    fn only_server_errors_are_retried() {
        assert_eq!(classify(StatusCode::INTERNAL_SERVER_ERROR, None).0, Outcome::Retry);
        assert_eq!(classify(StatusCode::BAD_GATEWAY, None).0, Outcome::Retry);
        assert_eq!(classify(StatusCode::BAD_REQUEST, reply(false, "no")).0, Outcome::Failed);
        assert_eq!(classify(StatusCode::UNAUTHORIZED, None).0, Outcome::Failed);
    }

    /// Against a stand-in API on localhost: the request carries the key, the
    /// spec's content type, the winner's intra id and every participant's, and
    /// the answer is read back.
    #[tokio::test]
    async fn sends_what_the_spec_asks_for() {
        use axum::http::HeaderMap;
        use axum::routing::post;
        use axum::{Json, Router};

        async fn give(headers: HeaderMap, body: String) -> (StatusCode, Json<serde_json::Value>) {
            let ok = headers.get(AUTHORIZATION).is_some_and(|v| v == "secret")
                && headers
                    .get(CONTENT_TYPE)
                    .is_some_and(|v| v == "application/json; charset=utf-8")
                && body == r#"{"winner_user_id":4242,"participant_user_ids":[7,19,4242]}"#;
            if ok {
                (StatusCode::OK, Json(serde_json::json!({ "success": true, "message": "given" })))
            } else {
                (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "success": false, "message": body })))
            }
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route("/api/give_points", post(give));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let api = Api {
            url: format!("http://{addr}/"),
            key: "secret".to_string(),
        };
        let payout = Payout {
            winner_id: 4242,
            participant_ids: vec![7, 19, 4242],
        };
        let (o, detail) = give_points(&reqwest::Client::new(), &api, &payout).await;
        assert_eq!(o, Outcome::Given, "{detail}");
    }
}
