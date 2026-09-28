use anyhow::{Context, Result};

/// Everything the app needs from the environment.
#[derive(Debug, Clone)]
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
        })
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
            client_id: String::new(),
            client_secret: String::new(),
            redirect_uri: String::new(),
            database_url: String::new(),
            bind_addr: String::new(),
            secure_cookies: false,
            admin_logins: Vec::new(),
            allowed_campus_ids: ids.to_vec(),
            points_api_key: None,
            points_api_url: String::new(),
        }
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
