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
        })
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

fn req(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("missing required env var {key}"))
}

fn opt(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}
