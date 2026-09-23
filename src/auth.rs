//! The 42 intra OAuth2 authorization-code flow, plus session cookie helpers.

use anyhow::{bail, Context, Result};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use rand::distributions::Alphanumeric;
use rand::Rng;
use reqwest::Url;
use serde::Deserialize;
use time::Duration as CookieDuration;

use crate::config::Config;

pub const AUTHORIZE_URL: &str = "https://api.intra.42.fr/oauth/authorize";
pub const TOKEN_URL: &str = "https://api.intra.42.fr/oauth/token";
pub const ME_URL: &str = "https://api.intra.42.fr/v2/me";

pub const SESSION_COOKIE: &str = "ig42_session";
/// Parks an admin's own session token while they browse as a demo account, so
/// they can get back without signing in through 42 again. Test instances only.
pub const ADMIN_RETURN_COOKIE: &str = "ig42_admin_return";
pub const SESSION_TTL_DAYS: i64 = 30;

/// 32 alphanumeric characters from the OS RNG: used for both session tokens
/// and OAuth `state` values.
pub fn random_token() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

pub fn authorize_url(cfg: &Config, state: &str) -> Result<String> {
    let mut url = Url::parse(AUTHORIZE_URL).context("parsing the 42 authorize URL")?;
    url.query_pairs_mut()
        .append_pair("client_id", &cfg.client_id)
        .append_pair("redirect_uri", &cfg.redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", "public")
        .append_pair("state", state);
    Ok(url.into())
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Debug, Deserialize)]
pub struct IntraUser {
    pub id: i64,
    pub login: String,
    pub displayname: Option<String>,
    pub image: Option<IntraImage>,
    /// Every campus the account has belonged to, with names.
    #[serde(default)]
    pub campus: Vec<IntraCampus>,
    /// Links to those campuses; exactly one is flagged as the home campus.
    #[serde(default)]
    pub campus_users: Vec<IntraCampusUser>,
}

#[derive(Debug, Deserialize)]
pub struct IntraImage {
    pub link: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct IntraCampus {
    pub id: i64,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct IntraCampusUser {
    pub campus_id: i64,
    #[serde(default)]
    pub is_primary: bool,
}

impl IntraUser {
    pub fn display_name(&self) -> &str {
        self.displayname
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&self.login)
    }

    pub fn image_url(&self) -> Option<&str> {
        self.image.as_ref()?.link.as_deref()
    }

    /// The home campus. A student visiting another campus keeps their own as
    /// primary, so visitors are judged by where they actually study. Accounts
    /// with no primary flag but a single campus fall back to that one.
    pub fn primary_campus_id(&self) -> Option<i64> {
        self.campus_users
            .iter()
            .find(|cu| cu.is_primary)
            .map(|cu| cu.campus_id)
            .or(match self.campus.as_slice() {
                [only] => Some(only.id),
                _ => None,
            })
    }

    /// Name of the home campus, for telling a turned-away player why.
    pub fn primary_campus_name(&self) -> Option<&str> {
        let id = self.primary_campus_id()?;
        self.campus.iter().find(|c| c.id == id)?.name.as_deref()
    }
}

pub async fn exchange_code(http: &reqwest::Client, cfg: &Config, code: &str) -> Result<String> {
    let resp = http
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", cfg.client_id.as_str()),
            ("client_secret", cfg.client_secret.as_str()),
            ("code", code),
            ("redirect_uri", cfg.redirect_uri.as_str()),
        ])
        .send()
        .await
        .context("calling the 42 token endpoint")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("42 token endpoint returned {status}: {body}");
    }

    Ok(resp
        .json::<TokenResponse>()
        .await
        .context("decoding the 42 token response")?
        .access_token)
}

pub async fn fetch_me(http: &reqwest::Client, access_token: &str) -> Result<IntraUser> {
    let resp = http
        .get(ME_URL)
        .bearer_auth(access_token)
        .send()
        .await
        .context("calling /v2/me")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("42 /v2/me returned {status}: {body}");
    }

    resp.json::<IntraUser>().await.context("decoding /v2/me")
}

pub fn session_cookie(token: String, secure: bool) -> Cookie<'static> {
    let mut cookie = Cookie::new(SESSION_COOKIE, token);
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_secure(secure);
    cookie.set_path("/");
    cookie.set_max_age(CookieDuration::days(SESSION_TTL_DAYS));
    cookie
}

/// A removal cookie must match the path the original was set on.
pub fn clearing_cookie() -> Cookie<'static> {
    let mut cookie = Cookie::new(SESSION_COOKIE, "");
    cookie.set_path("/");
    cookie
}

pub fn session_token(jar: &CookieJar) -> Option<String> {
    jar.get(SESSION_COOKIE).map(|c| c.value().to_string())
}

pub fn admin_return_cookie(token: String, secure: bool) -> Cookie<'static> {
    let mut cookie = Cookie::new(ADMIN_RETURN_COOKIE, token);
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_secure(secure);
    cookie.set_path("/");
    cookie.set_max_age(CookieDuration::days(SESSION_TTL_DAYS));
    cookie
}

pub fn clearing_admin_return_cookie() -> Cookie<'static> {
    let mut cookie = Cookie::new(ADMIN_RETURN_COOKIE, "");
    cookie.set_path("/");
    cookie
}

pub fn admin_return_token(jar: &CookieJar) -> Option<String> {
    jar.get(ADMIN_RETURN_COOKIE).map(|c| c.value().to_string())
}

#[cfg(test)]
mod tests {
    use super::IntraUser;

    fn user(json: &str) -> IntraUser {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn the_primary_campus_wins_over_a_visited_one() {
        let u = user(
            r#"{"id":1,"login":"a","displayname":"A","image":null,
                "campus":[{"id":1,"name":"Paris"},{"id":53,"name":"Vienna"}],
                "campus_users":[{"campus_id":1,"is_primary":false},
                                {"campus_id":53,"is_primary":true}]}"#,
        );
        assert_eq!(u.primary_campus_id(), Some(53));
        assert_eq!(u.primary_campus_name(), Some("Vienna"));
    }

    #[test]
    fn a_single_campus_counts_even_without_a_primary_flag() {
        let u = user(r#"{"id":1,"login":"a","campus":[{"id":53,"name":"Vienna"}]}"#);
        assert_eq!(u.primary_campus_id(), Some(53));
    }

    #[test]
    fn no_campus_information_means_no_campus() {
        let u = user(r#"{"id":1,"login":"a"}"#);
        assert_eq!(u.primary_campus_id(), None);
        assert_eq!(u.primary_campus_name(), None);
    }
}
