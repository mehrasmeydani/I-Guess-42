//! Held guesses: a number a signed-out visitor typed into the global round,
//! kept on the server until they have signed in, then locked for them.
//!
//! Kept in memory, keyed by a random token in a short-lived cookie: it is
//! session state, not game data, and a restart losing a few minutes' worth of
//! held numbers is fine - their owners are asked again. Never in the URL,
//! where it would end up in logs and browser history.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};

pub const COOKIE: &str = "ig42_held";
/// Long enough to make an account, short enough not to linger.
const KEEP_FOR: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    /// The round it was typed for: a number held over the deadline is
    /// discarded, never moved to the next round.
    pub round: String,
    pub value: i64,
}

#[derive(Default)]
pub struct HeldGuesses {
    held: Mutex<HashMap<String, (Held, Instant)>>,
}

impl HeldGuesses {
    /// Holds a number and returns the token that finds it again.
    pub fn hold(&self, round: String, value: i64) -> String {
        let token = crate::auth::random_token();
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        held.retain(|_, (_, at)| at.elapsed() < KEEP_FOR);
        held.insert(token.clone(), (Held { round, value }, Instant::now()));
        token
    }

    /// What a token holds, leaving it in place.
    pub fn peek(&self, token: &str) -> Option<Held> {
        let held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        held.get(token)
            .filter(|(_, at)| at.elapsed() < KEEP_FOR)
            .map(|(h, _)| h.clone())
    }

    /// What a token holds, removing it: whatever happens next, it is used
    /// at most once.
    pub fn take(&self, token: &str) -> Option<Held> {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        held.remove(token)
            .filter(|(_, at)| at.elapsed() < KEEP_FOR)
            .map(|(h, _)| h)
    }
}

pub fn cookie(token: String, secure: bool) -> Cookie<'static> {
    let mut c = Cookie::new(COOKIE, token);
    c.set_http_only(true);
    c.set_same_site(SameSite::Lax);
    c.set_secure(secure);
    c.set_path("/");
    c.set_max_age(time::Duration::seconds(KEEP_FOR.as_secs() as i64));
    c
}

pub fn clearing_cookie() -> Cookie<'static> {
    let mut c = Cookie::new(COOKIE, "");
    c.set_path("/");
    c
}

pub fn token(jar: &CookieJar) -> Option<String> {
    jar.get(COOKIE).map(|c| c.value().to_string()).filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_number_is_found_by_its_token_and_taken_once() {
        let h = HeldGuesses::default();
        let t = h.hold("2026-10-21".into(), 7);
        let other = h.hold("2026-10-21".into(), 9);
        assert_ne!(t, other);
        let want = Held { round: "2026-10-21".into(), value: 7 };
        assert_eq!(h.peek(&t), Some(want.clone()));
        assert_eq!(h.peek(&t), Some(want.clone()), "peeking leaves it");
        assert_eq!(h.take(&t), Some(want));
        assert_eq!(h.take(&t), None, "taken once");
        assert_eq!(h.peek("made-up"), None);
        assert_eq!(h.peek(&other).map(|x| x.value), Some(9));
    }
}
