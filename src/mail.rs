//! Sending the few mails the site sends: the link that verifies an email
//! address, the link that resets a password, and the note that an address
//! already has an account. Plain text, no tracking, through Resend's HTTP API
//! (https://resend.com/docs/api-reference/emails/send-email).
//!
//! Without a Resend key nothing leaves the machine: a test instance writes
//! each mail to the log instead, links and all, so sign-up can be tried
//! end to end locally.

#[cfg(test)]
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::Serialize;

use crate::config::Config;
use crate::db::{self, Db};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mail {
    pub to: String,
    pub subject: String,
    pub text: String,
    /// What kind of mail, for the daily count and the log: "verify", "reset"
    /// or "exists".
    pub purpose: &'static str,
}

#[derive(Clone)]
pub enum Mailer {
    Resend { url: String, key: String, from: String },
    /// Test instances without a key: every mail goes to the log.
    Log,
    /// Unit tests: every mail is kept for the test to read.
    #[cfg(test)]
    Memory(Arc<Mutex<Vec<Mail>>>),
    /// A live site without a key: email sign-up is off, nothing is sent.
    Off,
}

/// Mails the daily limit leaves room for beyond it, so that a password reset
/// still goes out on a day sign-ups used the allowance up.
const RESET_HEADROOM: i64 = 10;

#[derive(Serialize)]
struct ResendBody<'a> {
    from: &'a str,
    to: [&'a str; 1],
    subject: &'a str,
    text: &'a str,
}

impl Mailer {
    pub fn from_config(cfg: &Config) -> Self {
        match (&cfg.resend_api_key, cfg.test_mode()) {
            (Some(key), _) => Mailer::Resend {
                url: cfg.resend_api_url.trim_end_matches('/').to_string(),
                key: key.clone(),
                from: cfg.mail_from.clone(),
            },
            (None, true) => Mailer::Log,
            (None, false) => Mailer::Off,
        }
    }

    #[cfg(test)]
    pub fn memory() -> (Self, Arc<Mutex<Vec<Mail>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        (Mailer::Memory(sent.clone()), sent)
    }

    /// Sends `mail` unless today's allowance is used up, in which case it
    /// returns `Ok(false)` and sends nothing. Resets get a little headroom.
    pub async fn send(
        &self,
        db: &Db,
        http: &reqwest::Client,
        daily_limit: i64,
        mail: &Mail,
    ) -> Result<bool> {
        if matches!(self, Mailer::Off) {
            bail!("no mail service is configured");
        }
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let sent = db::mails_sent_since(db, &format!("{today}T00:00:00Z")).await?;
        let limit = match mail.purpose {
            "reset" => daily_limit + RESET_HEADROOM,
            _ => daily_limit,
        };
        if sent >= limit {
            tracing::warn!(purpose = mail.purpose, sent, "daily mail limit reached; not sending");
            return Ok(false);
        }

        match self {
            Mailer::Resend { url, key, from } => {
                let resp = http
                    .post(format!("{url}/emails"))
                    .bearer_auth(key)
                    .json(&ResendBody {
                        from,
                        to: [&mail.to],
                        subject: &mail.subject,
                        text: &mail.text,
                    })
                    .send()
                    .await
                    .context("calling the mail service")?;
                let status = resp.status();
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    bail!("the mail service answered {status}: {body}");
                }
            }
            Mailer::Log => {
                tracing::info!(
                    to = %mail.to,
                    subject = %mail.subject,
                    "mail not sent (no RESEND_API_KEY on this test instance):\n{}",
                    mail.text
                );
            }
            #[cfg(test)]
            Mailer::Memory(sent) => sent.lock().unwrap().push(mail.clone()),
            Mailer::Off => unreachable!("refused above"),
        }
        db::log_mail(db, mail.purpose).await?;
        Ok(true)
    }
}

// ------------------------------------------------------------ the mails

pub fn verify(to: &str, link: &str) -> Mail {
    Mail {
        to: to.to_string(),
        subject: "Confirm your email for i guess low".to_string(),
        text: format!(
            "Hi,\n\n\
             open this link to confirm your email address and start playing:\n\n\
             {link}\n\n\
             It works once and for 24 hours. If you didn't sign up for i guess low, \
             ignore this mail and nothing happens.\n"
        ),
        purpose: "verify",
    }
}

pub fn reset(to: &str, link: &str) -> Mail {
    Mail {
        to: to.to_string(),
        subject: "Reset your i guess low password".to_string(),
        text: format!(
            "Hi,\n\n\
             someone asked to reset the password for this address. To pick a new one, open:\n\n\
             {link}\n\n\
             It works once and for one hour. If it wasn't you, ignore this mail: \
             your password stays as it is.\n"
        ),
        purpose: "reset",
    }
}

/// Sent instead of a second account when someone signs up with an address
/// that has one, so the sign-up form never tells anyone which addresses are
/// registered.
pub fn exists(to: &str, base: &str) -> Mail {
    Mail {
        to: to.to_string(),
        subject: "You already have an i guess low account".to_string(),
        text: format!(
            "Hi,\n\n\
             someone tried to sign up for i guess low with this address, which already \
             has an account. Sign in at {base}/login the way you did before: with your \
             password, or with GitHub, Google or 42. Forgot your password? Reset it at \
             {base}/forgot. Want a password on an account you made with GitHub or \
             Google? Sign in that way and add one on your account page.\n\n\
             If it wasn't you, ignore this mail.\n"
        ),
        purpose: "exists",
    }
}
