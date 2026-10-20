//! The rules for what people type when they make an account, and the
//! hashing that keeps passwords, emailed links and addresses out of the
//! database in readable form.

use std::sync::LazyLock;

use anyhow::{anyhow, Context, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

// ------------------------------------------------------------- usernames

pub const USERNAME_MIN: usize = 3;
pub const USERNAME_MAX: usize = 20;

/// Names nobody may pick: they would read as the site speaking, or as one of
/// the stand-ins a test instance invents. 42 logins are reserved too, but
/// that list lives in the database (`db::username_available`).
const RESERVED: &[&str] = &[
    "admin", "administrator", "anonymous", "deleted", "help", "iguesslow", "mod",
    "moderator", "null", "root", "staff", "support", "system", "you",
];
const RESERVED_PREFIXES: &[&str] = &["bot-", "deleted-", "test"];

/// Why a username was refused, as the message shown next to the field.
pub fn username_problem(name: &str) -> Option<&'static str> {
    let len = name.chars().count();
    if !(USERNAME_MIN..=USERNAME_MAX).contains(&len) {
        return Some("Usernames are 3 to 20 characters long.");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Some("Usernames use lowercase letters, digits, - and _ only.");
    }
    if !name.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return Some("Usernames start with a letter or a digit.");
    }
    if RESERVED.contains(&name) || RESERVED_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return Some("That username is reserved.");
    }
    None
}

/// What someone typed, as a username would be stored: trimmed, lowercase.
pub fn normalize_username(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// A username made from something else (a GitHub login, the start of an
/// email address): lowercase, only allowed characters, cut to length, and
/// padded if too short. It can still be taken or reserved; callers number it.
pub fn username_from(raw: &str) -> String {
    let mut name: String = raw
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' })
        .collect();
    name = name.trim_matches(|c| c == '-' || c == '_').to_string();
    name.truncate(USERNAME_MAX - 3); // room for a "-99" suffix
    if name.chars().count() < USERNAME_MIN {
        name = format!("player-{name}").trim_end_matches('-').to_string();
    }
    if username_problem(&name) == Some("That username is reserved.") {
        name = format!("p-{name}");
        name.truncate(USERNAME_MAX - 3);
    }
    name
}

// ---------------------------------------------------------------- emails

/// Disposable inbox providers, from the CC0 `disposable-email-domains` list
/// (https://github.com/disposable-email-domains/disposable-email-domains).
static DISPOSABLE: LazyLock<std::collections::HashSet<&'static str>> = LazyLock::new(|| {
    include_str!("lists/disposable_domains.txt")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect()
});

/// The email as it is stored and compared: trimmed and lowercase, or why it
/// was refused.
pub fn normalize_email(raw: &str) -> Result<String, &'static str> {
    let email = raw.trim().to_lowercase();
    let Some((local, domain)) = email.split_once('@') else {
        return Err("That doesn't look like an email address.");
    };
    let plausible = !local.is_empty()
        && email.len() <= 254
        && !domain.contains('@')
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !email.chars().any(|c| c.is_whitespace() || c.is_control());
    if !plausible {
        return Err("That doesn't look like an email address.");
    }
    if is_disposable(domain) {
        return Err("Throwaway email addresses can't be used here.");
    }
    Ok(email)
}

/// True for a listed domain and for any subdomain of one.
fn is_disposable(domain: &str) -> bool {
    let mut rest = domain;
    loop {
        if DISPOSABLE.contains(rest) {
            return true;
        }
        match rest.split_once('.') {
            Some((_, parent)) if parent.contains('.') => rest = parent,
            _ => return false,
        }
    }
}

// ------------------------------------------------------------- passwords

pub const PASSWORD_MIN: usize = 10;
/// An upper bound, so hashing cannot be used to burn memory and time.
pub const PASSWORD_MAX: usize = 128;

/// Passwords of at least [`PASSWORD_MIN`] characters from the most common
/// ones in leaked lists (SecLists, MIT). Shorter ones are refused anyway.
static COMMON: LazyLock<std::collections::HashSet<&'static str>> = LazyLock::new(|| {
    include_str!("lists/common_passwords.txt")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect()
});

/// Why a password was refused. Length and "not an obvious one" only, after
/// NIST 800-63B: no rules about symbols or digits, which make passwords
/// harder to remember and no harder to guess.
pub fn password_problem(password: &str, email: &str, username: &str) -> Option<&'static str> {
    let len = password.chars().count();
    if len < PASSWORD_MIN {
        return Some("Passwords are at least 10 characters long.");
    }
    if len > PASSWORD_MAX {
        return Some("Passwords are at most 128 characters long.");
    }
    let lower = password.to_lowercase();
    let local = email.split('@').next().unwrap_or("");
    if lower == email || lower == username || (!local.is_empty() && lower == local) {
        return Some("Your password can't be your email address or username.");
    }
    if COMMON.contains(lower.as_str()) {
        return Some("That password is one of the most common ones. Pick another.");
    }
    None
}

/// OWASP's baseline for argon2id: 19 MiB, two passes, one lane.
fn argon2() -> Argon2<'static> {
    let params = Params::new(19 * 1024, 2, 1, None).expect("valid argon2 parameters");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// At most four hashes at once: each takes 19 MiB and tens of milliseconds
/// of a core, and a burst of sign-ins must not take the server with it.
static HASHING: Semaphore = Semaphore::const_new(4);

pub async fn hash_password(password: &str) -> Result<String> {
    let password = password.to_string();
    let _permit = HASHING.acquire().await.context("hashing semaphore closed")?;
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
        argon2()
            .hash_password(password.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| anyhow!("hashing a password: {e}"))
    })
    .await?
}

/// Checks a password against a stored hash. `None` for the hash still does
/// the work, against a fixed decoy, so the answer takes as long whether or
/// not the account exists.
pub async fn verify_password(password: &str, stored: Option<&str>) -> Result<bool> {
    let password = password.to_string();
    let real = stored.is_some();
    let stored = stored.unwrap_or(&DECOY).to_string();
    let _permit = HASHING.acquire().await.context("hashing semaphore closed")?;
    tokio::task::spawn_blocking(move || {
        let hash = PasswordHash::new(&stored).map_err(|e| anyhow!("reading a password hash: {e}"))?;
        Ok(argon2().verify_password(password.as_bytes(), &hash).is_ok() && real)
    })
    .await?
}

/// A hash of nothing anyone will type, made once with the real parameters.
static DECOY: LazyLock<String> = LazyLock::new(|| {
    let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
    argon2()
        .hash_password(crate::auth::random_token().as_bytes(), &salt)
        .expect("hashing the decoy")
        .to_string()
});

// ------------------------------------------------------------ link tokens

/// A token for an emailed link and the hash that is stored for it.
pub fn new_link_token() -> (String, String) {
    let token = format!("{}{}", crate::auth::random_token(), crate::auth::random_token());
    let hash = hash_link_token(&token);
    (token, hash)
}

pub fn hash_link_token(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

// ------------------------------------------------------------- addresses

/// A keyed hash of a client address: the same address gives the same short
/// string within a calendar month, so a moderator can see that several
/// accounts came from one network, and nothing about where that network is.
/// The month is part of the key, so the strings cannot be followed for longer.
pub fn ip_hash(key: &str, ip: std::net::IpAddr, month: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(format!("{key}:{month}").as_bytes())
        .expect("HMAC takes keys of any length");
    mac.update(ip.to_string().as_bytes());
    hex(&mac.finalize().into_bytes())[..16].to_string()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames_follow_the_rules() {
        assert_eq!(username_problem("megardes"), None);
        assert_eq!(username_problem("a_b-9"), None);
        assert!(username_problem("ab").is_some(), "too short");
        assert!(username_problem(&"a".repeat(21)).is_some(), "too long");
        assert!(username_problem("Mega").is_some(), "uppercase is normalised first");
        assert!(username_problem("me gardes").is_some());
        assert!(username_problem("mé").is_some());
        assert!(username_problem("-lead").is_some());
        for taken in ["admin", "mod", "bot-001", "testing", "deleted-4", "system"] {
            assert_eq!(username_problem(taken), Some("That username is reserved."), "{taken}");
        }
        assert_eq!(normalize_username("  MeGardes "), "megardes");
    }

    #[test]
    fn usernames_made_from_elsewhere_are_always_valid_shapes() {
        for raw in ["Octo-Cat", "a", "", "ü", "john.doe+tag", "admin", "Bot-7", &"x".repeat(60), "__x__"] {
            let name = username_from(raw);
            assert_eq!(username_problem(&name), None, "{raw:?} became {name:?}");
            // Room left for a numbering suffix.
            assert!(name.len() <= USERNAME_MAX - 3, "{name}");
        }
        assert_eq!(username_from("Octo-Cat"), "octo-cat");
        assert_eq!(username_from("john.doe"), "john-doe");
    }

    #[test]
    fn emails_are_normalised_and_throwaways_refused() {
        assert_eq!(normalize_email("  Ann@Example.COM "), Ok("ann@example.com".to_string()));
        for bad in ["", "ann", "ann@", "@example.com", "ann@example", "a@b@c.com", "ann@.com", "ann @x.com"] {
            assert!(normalize_email(bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            normalize_email("x@mailinator.com"),
            Err("Throwaway email addresses can't be used here.")
        );
        assert!(normalize_email("x@inbox.mailinator.com").is_err(), "subdomains too");
        assert!(normalize_email("x@gmail.com").is_ok());
    }

    #[test]
    fn passwords_need_length_and_not_to_be_obvious() {
        assert_eq!(password_problem("correct horse battery", "ann@x.com", "ann"), None);
        assert!(password_problem("short", "ann@x.com", "ann").is_some());
        assert!(password_problem(&"a".repeat(129), "ann@x.com", "ann").is_some());
        assert!(password_problem(&"a".repeat(128), "ann@x.com", "ann").is_none());
        assert!(password_problem("1234567890", "ann@x.com", "ann").is_some(), "common");
        assert!(password_problem("annabelle@x.com", "annabelle@x.com", "ann").is_some(), "own email");
        assert!(password_problem("AnnaBelle1", "annabelle1@x.com", "zed").is_some(), "own email name");
        assert!(password_problem("supersecretname", "a@x.com", "supersecretname").is_some(), "own username");
        // Length is in characters, not bytes.
        assert!(password_problem("ääääääääää", "a@x.com", "ann").is_none());
    }

    #[tokio::test]
    async fn a_hash_verifies_its_password_and_nothing_else() {
        let hash = hash_password("correct horse battery").await.unwrap();
        assert!(hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{hash}");
        assert!(verify_password("correct horse battery", Some(&hash)).await.unwrap());
        assert!(!verify_password("correct horse batterY", Some(&hash)).await.unwrap());
        // Salted: the same password never hashes the same way twice.
        assert_ne!(hash, hash_password("correct horse battery").await.unwrap());
        // No account: always false, but the work is still done.
        assert!(!verify_password("anything", None).await.unwrap());
    }

    #[test]
    fn link_tokens_are_long_and_stored_only_as_hashes() {
        let (token, hash) = new_link_token();
        assert_eq!(token.len(), 64);
        assert_eq!(hash.len(), 64);
        assert_ne!(token, hash);
        assert_eq!(hash_link_token(&token), hash);
        assert_ne!(new_link_token().0, token);
    }

    #[test]
    fn address_hashes_are_stable_within_a_month_only() {
        let ip: std::net::IpAddr = "203.0.113.7".parse().unwrap();
        let other: std::net::IpAddr = "203.0.113.8".parse().unwrap();
        let a = ip_hash("k", ip, "2026-10");
        assert_eq!(a.len(), 16);
        assert_eq!(a, ip_hash("k", ip, "2026-10"));
        assert_ne!(a, ip_hash("k", other, "2026-10"));
        assert_ne!(a, ip_hash("k", ip, "2026-11"), "a new month, a new string");
        assert_ne!(a, ip_hash("other key", ip, "2026-10"));
    }
}
