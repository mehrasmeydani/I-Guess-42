use anyhow::{Context, Result};

/// Everything the app needs from the environment.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// 42 OAuth application UID.
    pub client_id: String,
    /// 42 OAuth application secret.
    pub client_secret: String,
    /// Must match a redirect URI registered on the intra application, byte for byte.
    pub redirect_uri: String,
    pub database_url: String,
    pub bind_addr: String,
    /// Set the `Secure` flag on the session cookie. Turn off for plain-http local dev.
    pub secure_cookies: bool,
    /// 42 logins allowed into `/admin`. Empty disables test mode outright, and
    /// the admin routes then answer 404 rather than 403 so a live deployment
    /// does not advertise that they exist.
    pub admin_logins: Vec<String>,
    /// 42 campus ids whose students may sign in, matched against the primary
    /// campus on the intra account. Empty lets every campus in.
    pub allowed_campus_ids: Vec<i64>,
    /// Key for the 42 Vienna coalition points API. None (unset or empty)
    /// turns payouts off; see points.rs.
    pub points_api_key: Option<String>,
    pub points_api_url: String,
    /// Where the site lives, e.g. `https://iguesslow.com`, for the links in
    /// emails and the OAuth redirect URIs. Defaults to the origin of
    /// FT_REDIRECT_URI, which already names it.
    pub public_url: String,
    /// Resend (https://resend.com) API key. None: no mail is sent; email
    /// sign-up is off on a live site, and a test instance prints mails to the
    /// log instead.
    pub resend_api_key: Option<String>,
    pub resend_api_url: String,
    /// The From header of every mail, e.g. `i guess low <noreply@iguesslow.com>`.
    pub mail_from: String,
    /// Mails per UTC day, kept under the mail service's free limit.
    pub mail_daily_limit: i64,
    /// Cloudflare Turnstile keys for the sign-up form. None: no CAPTCHA.
    pub turnstile_site_key: Option<String>,
    pub turnstile_secret: Option<String>,
    pub turnstile_verify_url: String,
    /// Key for hashing client addresses (see `signin::ip_hash`). None: no
    /// address-derived data is stored at all.
    pub ip_hash_key: Option<String>,
    /// Believe X-Forwarded-For. Only true behind a proxy that sets it, as
    /// Caddy does in compose; otherwise any visitor could claim any address.
    pub trust_proxy: bool,
    /// The 42 intra API, `https://api.intra.42.fr`; only tests change it.
    pub ft_api_url: String,
    /// GitHub OAuth app. Unset: no GitHub button.
    pub github_client_id: Option<String>,
    pub github_client_secret: Option<String>,
    /// GitHub accounts younger than this many days cannot make an account
    /// here: a fresh one costs nothing, so it is the cheapest fake there is.
    pub github_min_age_days: i64,
    /// `https://github.com` (authorize, token) and `https://api.github.com`.
    pub github_web_url: String,
    pub github_api_url: String,
    /// Google OAuth client. Unset: no Google button.
    pub google_client_id: Option<String>,
    pub google_client_secret: Option<String>,
    pub google_auth_url: String,
    pub google_token_url: String,
    pub google_userinfo_url: String,
    /// 42 logins who may moderate the global pool at /mod, on the live site
    /// too. 42 logins and not usernames, because anyone can pick a username.
    /// Empty: /mod answers 404. On a test instance the admins can as well.
    pub moderators: Vec<String>,
    /// ENVIRONMENT: `test` or `production`. Decides the test-instance
    /// notice, which imprint is shown, and how data is labelled. Unset: a
    /// test instance exactly when ADMIN_LOGINS is set.
    pub environment: Option<String>,
    /// Keys the anonymous "player #3fa2" labels. ANON_LABEL_KEY, or derived
    /// from the 42 client secret so it is secret and stable without one more
    /// setting.
    pub anon_key: String,
    /// Who runs the site, for /imprint.
    pub imprint_name: Option<String>,
    pub imprint_address: Option<String>,
    pub imprint_email: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            client_id: req("FT_CLIENT_ID")?,
            client_secret: req("FT_CLIENT_SECRET")?,
            redirect_uri: opt("FT_REDIRECT_URI", "http://localhost:3000/auth/callback"),
            database_url: opt("DATABASE_URL", "sqlite://data/game.db"),
            bind_addr: opt("BIND_ADDR", "127.0.0.1:3000"),
            secure_cookies: opt("SECURE_COOKIES", "false") == "true",
            admin_logins: opt("ADMIN_LOGINS", "")
                .split(',')
                .map(|l| l.trim().to_lowercase())
                .filter(|l| !l.is_empty())
                .collect(),
            allowed_campus_ids: parse_ids(&opt("ALLOWED_CAMPUS_IDS", DEFAULT_CAMPUS_IDS))
                .context("ALLOWED_CAMPUS_IDS must be comma-separated campus ids")?,
            points_api_key: Some(opt("IGLCP_API_KEY", "").trim().to_string())
                .filter(|k| !k.is_empty()),
            points_api_url: opt("IGLCP_API_URL", crate::points::DEFAULT_API_URL),
            public_url: match nonempty("PUBLIC_URL") {
                Some(url) => url.trim_end_matches('/').to_string(),
                None => origin(&opt("FT_REDIRECT_URI", "http://localhost:3000/auth/callback")),
            },
            resend_api_key: nonempty("RESEND_API_KEY"),
            resend_api_url: opt("RESEND_API_URL", "https://api.resend.com"),
            mail_from: opt("MAIL_FROM", "i guess low <noreply@iguesslow.com>"),
            mail_daily_limit: opt("MAIL_DAILY_LIMIT", "90")
                .trim()
                .parse()
                .context("MAIL_DAILY_LIMIT must be a whole number")?,
            turnstile_site_key: nonempty("TURNSTILE_SITE_KEY"),
            turnstile_secret: nonempty("TURNSTILE_SECRET"),
            turnstile_verify_url: opt(
                "TURNSTILE_VERIFY_URL",
                "https://challenges.cloudflare.com/turnstile/v0/siteverify",
            ),
            ip_hash_key: nonempty("IP_HASH_KEY"),
            trust_proxy: opt("TRUST_PROXY", "false") == "true",
            ft_api_url: opt("FT_API_URL", "https://api.intra.42.fr"),
            github_client_id: nonempty("GITHUB_CLIENT_ID"),
            github_client_secret: nonempty("GITHUB_CLIENT_SECRET"),
            github_min_age_days: opt("GITHUB_MIN_ACCOUNT_AGE_DAYS", "30")
                .trim()
                .parse()
                .context("GITHUB_MIN_ACCOUNT_AGE_DAYS must be a whole number")?,
            github_web_url: opt("GITHUB_WEB_URL", "https://github.com"),
            github_api_url: opt("GITHUB_API_URL", "https://api.github.com"),
            google_client_id: nonempty("GOOGLE_CLIENT_ID"),
            google_client_secret: nonempty("GOOGLE_CLIENT_SECRET"),
            google_auth_url: opt("GOOGLE_AUTH_URL", "https://accounts.google.com/o/oauth2/v2/auth"),
            google_token_url: opt("GOOGLE_TOKEN_URL", "https://oauth2.googleapis.com/token"),
            google_userinfo_url: opt(
                "GOOGLE_USERINFO_URL",
                "https://openidconnect.googleapis.com/v1/userinfo",
            ),
            moderators: opt("MODERATORS", "")
                .split(',')
                .map(|l| l.trim().to_lowercase())
                .filter(|l| !l.is_empty())
                .collect(),
            environment: match nonempty("ENVIRONMENT").as_deref() {
                None => None,
                Some(e @ ("test" | "production")) => Some(e.to_string()),
                Some(other) => anyhow::bail!("ENVIRONMENT must be test or production, not {other:?}"),
            },
            anon_key: match nonempty("ANON_LABEL_KEY") {
                Some(k) => k,
                None => {
                    use sha2::Digest;
                    let secret = std::env::var("FT_CLIENT_SECRET").unwrap_or_default();
                    let digest = sha2::Sha256::digest(format!("anon-label:{secret}").as_bytes());
                    digest.iter().map(|b| format!("{b:02x}")).collect()
                }
            },
            imprint_name: nonempty("IMPRINT_NAME"),
            imprint_address: nonempty("IMPRINT_ADDRESS"),
            imprint_email: nonempty("IMPRINT_EMAIL"),
        })
    }

    /// GitHub's client id and secret, if both are set.
    pub fn github(&self) -> Option<(&str, &str)> {
        Some((self.github_client_id.as_deref()?, self.github_client_secret.as_deref()?))
    }

    /// Google's client id and secret, if both are set.
    pub fn google(&self) -> Option<(&str, &str)> {
        Some((self.google_client_id.as_deref()?, self.google_client_secret.as_deref()?))
    }

    /// Whether people can sign up with an email and a password here. It needs
    /// mail, for the verification link; a test instance logs its mails, so it
    /// always can.
    pub fn email_signup(&self) -> bool {
        self.resend_api_key.is_some() || self.test_mode()
    }

    /// True when a player whose primary campus is `campus_id` may sign in.
    pub fn campus_allowed(&self, campus_id: Option<i64>) -> bool {
        self.allowed_campus_ids.is_empty()
            || campus_id.is_some_and(|id| self.allowed_campus_ids.contains(&id))
    }

    /// Whether the front page says out loud that a round pays coalition points.
    /// True when this instance really pays them, and on a test instance, where
    /// the banner already says nothing here is live -- otherwise the note could
    /// not be seen anywhere before production.
    pub fn points_announced(&self) -> bool {
        self.points_api_key.is_some() || self.test_mode()
    }

    /// True when this instance is a test instance.
    pub fn test_mode(&self) -> bool {
        !self.admin_logins.is_empty()
    }

    /// Whether this is a test environment as far as visitors are concerned:
    /// the notice at the top, the test imprint, data labelled as test data.
    /// The admin tools still go by `test_mode` alone.
    pub fn test_env(&self) -> bool {
        match self.environment.as_deref() {
            Some("test") => true,
            Some("production") => false,
            _ => self.test_mode(),
        }
    }

    /// Whether the 42 login `login` may moderate the global pool.
    pub fn is_moderator(&self, login: &str) -> bool {
        let login = login.to_lowercase();
        self.moderators.contains(&login) || (self.test_mode() && self.is_admin(&login))
    }

    pub fn is_admin(&self, login: &str) -> bool {
        let login = login.to_lowercase();
        self.admin_logins.contains(&login)
    }
}

/// 42 Vienna. The round deadline is Vienna time, so the game is theirs by default.
const DEFAULT_CAMPUS_IDS: &str = "53";

/// `"53, 1,,"` -> `[53, 1]`. A typo is an error rather than a silently
/// dropped id, because a wrong list locks the right people out.
fn parse_ids(raw: &str) -> Result<Vec<i64>> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().with_context(|| format!("{s:?} is not a number")))
        .collect()
}

/// `scheme://host[:port]` of a URL, or the URL itself if it does not parse.
fn origin(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(u) => u.origin().ascii_serialization(),
        Err(_) => url.trim_end_matches('/').to_string(),
    }
}

/// The variable, trimmed, if it is set to something.
fn nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn req(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("missing required env var {key}"))
}

fn opt(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_campuses(ids: &[i64]) -> Config {
        Config {
            allowed_campus_ids: ids.to_vec(),
            ..Config::default()
        }
    }

    #[test]
    fn the_public_url_is_the_origin_of_the_redirect_uri() {
        assert_eq!(origin("https://iguesslow.com/auth/callback"), "https://iguesslow.com");
        assert_eq!(origin("http://localhost:3000/auth/callback"), "http://localhost:3000");
    }

    #[test]
    fn email_sign_up_needs_mail_or_a_test_instance() {
        let mut cfg = with_campuses(&[]);
        assert!(!cfg.email_signup());
        cfg.admin_logins = vec!["megardes".to_string()];
        assert!(cfg.email_signup(), "a test instance logs its mails");
        cfg.admin_logins.clear();
        cfg.resend_api_key = Some("re_x".to_string());
        assert!(cfg.email_signup());
    }

    #[test]
    fn campus_ids_parse_leniently_but_reject_typos() {
        assert_eq!(parse_ids("53").unwrap(), vec![53]);
        assert_eq!(parse_ids(" 53, 1 ,,").unwrap(), vec![53, 1]);
        assert!(parse_ids("").unwrap().is_empty());
        assert!(parse_ids("53,vienna").is_err());
    }

    #[test]
    fn only_listed_campuses_get_in() {
        let cfg = with_campuses(&[53]);
        assert!(cfg.campus_allowed(Some(53)));
        assert!(!cfg.campus_allowed(Some(1)));
        assert!(!cfg.campus_allowed(None));
    }

    #[test]
    fn the_points_note_shows_where_points_are_real_or_pretend() {
        let mut cfg = with_campuses(&[]);
        assert!(!cfg.points_announced(), "no key and no admins: nothing to say");
        cfg.points_api_key = Some("k".to_string());
        assert!(cfg.points_announced());
        cfg.points_api_key = None;
        cfg.admin_logins = vec!["megardes".to_string()];
        assert!(cfg.points_announced(), "a test instance shows the note to try it");
    }

    #[test]
    fn an_empty_list_lets_everyone_in() {
        let cfg = with_campuses(&[]);
        assert!(cfg.campus_allowed(Some(1)));
        assert!(cfg.campus_allowed(None));
    }
}
