//! The robot check on the sign-up form, self-hosted: no third party sees
//! anyone, and there are no keys to set up.
//!
//! - A proof-of-work puzzle. The server hands out a signed challenge; the
//!   browser (app.js) searches for a nonce whose SHA-256 of "salt:nonce"
//!   starts with `bits` zero bits - a fraction of a second for one person,
//!   real cost for a script making accounts by the thousand. Each challenge
//!   is good once, and for `MAX_AGE` only.
//! - Timing: the challenge says when the form was handed out; a form sent
//!   back faster than `MIN_SECONDS` was not filled in by a person.
//! - A honeypot: a field hidden from people (`website`) that form-filling
//!   scripts fill in.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

/// How long a challenge stays good.
pub const MAX_AGE: i64 = 15 * 60;
/// Faster than this from page to submit is a script.
pub const MIN_SECONDS: i64 = 2;
/// Zero bits the hash must start with: about 260 000 tries on average, a
/// fraction of a second on a laptop and about one on a phone.
pub const BITS: u32 = 18;

/// Why a form failed the check.
#[derive(Debug, PartialEq, Eq)]
pub enum Failed {
    /// No answer, a forged or broken challenge, or a wrong nonce.
    Puzzle,
    /// Older than `MAX_AGE`: the page sat open too long.
    Expired,
    /// Sent back faster than a person fills a form.
    TooFast,
    /// The hidden field was filled in.
    Honeypot,
    /// This challenge was used already.
    Reused,
}

fn sign(key: &str, body: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(format!("human:{key}").as_bytes()).expect("any key length works");
    mac.update(body.as_bytes());
    hex(&mac.finalize().into_bytes()[..16])
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A fresh challenge, "salt.issued.bits.signature", for a form handed out
/// at `now` (unix seconds).
pub fn issue(key: &str, now: i64, bits: u32) -> String {
    let salt = hex(&rand::random::<[u8; 8]>());
    let body = format!("{salt}.{now}.{bits}");
    format!("{body}.{}", sign(key, &body))
}

/// Leading zero bits of SHA-256("salt:nonce").
fn zero_bits(salt: &str, nonce: &str) -> u32 {
    let d = Sha256::digest(format!("{salt}:{nonce}").as_bytes());
    let mut n = 0;
    for b in d {
        if b == 0 {
            n += 8;
        } else {
            return n + b.leading_zeros();
        }
    }
    n
}

/// Checks a submitted form's answer. `first_use` is asked whether this
/// challenge's salt is being used for the first time (and records it), so a
/// solved challenge cannot be replayed.
pub fn check(key: &str, token: &str, nonce: &str, honeypot: &str, now: i64, first_use: impl FnOnce(&str) -> bool) -> Result<(), Failed> {
    if !honeypot.trim().is_empty() {
        return Err(Failed::Honeypot);
    }
    let parts: Vec<&str> = token.split('.').collect();
    let [salt, issued, bits, sig] = parts.as_slice() else {
        return Err(Failed::Puzzle);
    };
    let (Ok(issued), Ok(bits)) = (issued.parse::<i64>(), bits.parse::<u32>()) else {
        return Err(Failed::Puzzle);
    };
    if sign(key, &format!("{salt}.{issued}.{bits}")) != *sig {
        return Err(Failed::Puzzle);
    }
    if now - issued > MAX_AGE {
        return Err(Failed::Expired);
    }
    if now - issued < MIN_SECONDS {
        return Err(Failed::TooFast);
    }
    if nonce.is_empty() || nonce.len() > 20 || zero_bits(salt, nonce) < bits {
        return Err(Failed::Puzzle);
    }
    if !first_use(salt) {
        return Err(Failed::Reused);
    }
    Ok(())
}

/// A challenge handed out a few seconds ago and its answer, as a person's
/// browser sends them, for tests.
#[cfg(test)]
pub fn answered(key: &str, bits: u32) -> (String, String) {
    let token = issue(key, chrono::Utc::now().timestamp() - 5, bits);
    let nonce = solve(&token);
    (token, nonce)
}

/// What the browser does, for tests: the first nonce that solves `token`.
#[cfg(test)]
pub fn solve(token: &str) -> String {
    let parts: Vec<&str> = token.split('.').collect();
    let (salt, bits) = (parts[0], parts[2].parse::<u32>().unwrap());
    (0u64..).map(|n| n.to_string()).find(|n| zero_bits(salt, n) >= bits).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "k";

    fn fresh() -> impl FnMut(&str) -> bool {
        let mut seen = std::collections::HashSet::new();
        move |s: &str| seen.insert(s.to_string())
    }

    #[test]
    fn a_solved_challenge_passes_once() {
        let token = issue(KEY, 1000, 8);
        let nonce = solve(&token);
        let mut first = fresh();
        assert_eq!(check(KEY, &token, &nonce, "", 1005, &mut first), Ok(()));
        assert_eq!(check(KEY, &token, &nonce, "", 1006, &mut first), Err(Failed::Reused));
    }

    #[test]
    fn forged_wrong_slow_fast_and_honeypotted_forms_fail() {
        let token = issue(KEY, 1000, 8);
        let nonce = solve(&token);
        assert_eq!(check("other key", &token, &nonce, "", 1005, fresh()), Err(Failed::Puzzle), "signed with our key only");
        let easier = token.replacen(".8.", ".0.", 1);
        assert_eq!(check(KEY, &easier, "0", "", 1005, fresh()), Err(Failed::Puzzle), "the difficulty is signed too");
        assert_eq!(check(KEY, &token, "", "", 1005, fresh()), Err(Failed::Puzzle));
        assert_eq!(check(KEY, "garbage", &nonce, "", 1005, fresh()), Err(Failed::Puzzle));
        let wrong = (0u64..).map(|n| n.to_string()).find(|n| zero_bits(token.split('.').next().unwrap(), n) < 8).unwrap();
        assert_eq!(check(KEY, &token, &wrong, "", 1005, fresh()), Err(Failed::Puzzle));
        assert_eq!(check(KEY, &token, &nonce, "", 1001, fresh()), Err(Failed::TooFast));
        assert_eq!(check(KEY, &token, &nonce, "", 1000 + MAX_AGE + 1, fresh()), Err(Failed::Expired));
        assert_eq!(check(KEY, &token, &nonce, "http://spam", 1005, fresh()), Err(Failed::Honeypot));
    }

    #[test]
    fn the_hash_is_what_the_browser_computes() {
        // SHA-256("abc:1") starts 0x6b..., one leading zero bit; app.js must agree.
        let d = Sha256::digest(b"abc:1");
        assert_eq!(zero_bits("abc", "1"), d[0].leading_zeros());
    }
}
