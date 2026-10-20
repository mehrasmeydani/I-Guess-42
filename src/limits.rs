//! A small in-memory rate limiter: how many times something happened under a
//! key within a sliding window. Kept in the process, so a restart forgets it,
//! which is fine for slowing down password guessing and sign-up floods; it is
//! not an accounting system.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// One kind of limit: at most `max` events per `window`.
#[derive(Debug, Clone, Copy)]
pub struct Rule {
    pub max: usize,
    pub window: Duration,
}

const fn per_minutes(max: usize, minutes: u64) -> Rule {
    Rule {
        max,
        window: Duration::from_secs(minutes * 60),
    }
}

/// Failed password sign-ins for one email address.
pub const LOGIN_PER_EMAIL: Rule = per_minutes(5, 15);
/// Failed password sign-ins from one address, whichever emails it tries.
pub const LOGIN_PER_IP: Rule = per_minutes(20, 15);
/// New accounts from one address.
pub const SIGNUP_PER_IP: Rule = per_minutes(3, 60);
/// Verification and reset mails for one account or email address.
pub const MAIL_PER_ADDRESS: Rule = per_minutes(3, 60);

#[derive(Default)]
pub struct Limits {
    events: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl Limits {
    /// Whether another event under `key` would still be within `rule`,
    /// without recording one.
    pub fn allows(&self, key: &str, rule: Rule) -> bool {
        self.allows_at(key, rule, Instant::now())
    }

    /// Records an event under `key`.
    pub fn record(&self, key: &str) {
        self.record_at(key, Instant::now());
    }

    /// Checks and records in one step: true (and counted) if allowed.
    pub fn take(&self, key: &str, rule: Rule) -> bool {
        let now = Instant::now();
        if self.allows_at(key, rule, now) {
            self.record_at(key, now);
            true
        } else {
            false
        }
    }

    /// How long until `key` is allowed again under `rule`; zero if it is now.
    pub fn retry_after(&self, key: &str, rule: Rule) -> Duration {
        let now = Instant::now();
        let mut events = self.events.lock().unwrap_or_else(|e| e.into_inner());
        let Some(times) = events.get_mut(key) else {
            return Duration::ZERO;
        };
        prune(times, rule.window, now);
        if times.len() < rule.max {
            return Duration::ZERO;
        }
        // The window frees up when the oldest event that still counts ages out.
        let oldest = times[times.len() - rule.max];
        (oldest + rule.window).saturating_duration_since(now)
    }

    fn allows_at(&self, key: &str, rule: Rule, now: Instant) -> bool {
        let mut events = self.events.lock().unwrap_or_else(|e| e.into_inner());
        match events.get_mut(key) {
            Some(times) => {
                prune(times, rule.window, now);
                times.len() < rule.max
            }
            None => true,
        }
    }

    fn record_at(&self, key: &str, now: Instant) {
        let mut events = self.events.lock().unwrap_or_else(|e| e.into_inner());
        // Nothing is kept for longer than the longest window, so a key that
        // stops being used is dropped on its next visit or the next sweep.
        if events.len() > 50_000 {
            events.retain(|_, times| {
                prune(times, Duration::from_secs(60 * 60), now);
                !times.is_empty()
            });
        }
        events.entry(key.to_string()).or_default().push_back(now);
    }
}

fn prune(times: &mut VecDeque<Instant>, window: Duration, now: Instant) {
    while times.front().is_some_and(|t| now.duration_since(*t) >= window) {
        times.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: Rule = Rule {
        max: 3,
        window: Duration::from_secs(60),
    };

    #[test]
    fn allows_up_to_the_limit_and_then_refuses() {
        let l = Limits::default();
        assert!(l.take("k", RULE));
        assert!(l.take("k", RULE));
        assert!(l.take("k", RULE));
        assert!(!l.take("k", RULE), "the fourth inside the window");
        assert!(!l.allows("k", RULE));
        assert!(l.retry_after("k", RULE) > Duration::from_secs(50));
        // Other keys are counted on their own.
        assert!(l.take("other", RULE));
    }

    #[test]
    fn events_age_out_of_the_window() {
        let l = Limits::default();
        let start = Instant::now();
        for _ in 0..3 {
            l.record_at("k", start);
        }
        assert!(!l.allows_at("k", RULE, start + Duration::from_secs(59)));
        assert!(l.allows_at("k", RULE, start + Duration::from_secs(60)));
    }

    #[test]
    fn checking_does_not_count() {
        let l = Limits::default();
        for _ in 0..10 {
            assert!(l.allows("k", RULE));
        }
        assert_eq!(l.retry_after("k", RULE), Duration::ZERO);
    }
}
