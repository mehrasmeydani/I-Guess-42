//! Every message the server writes, in one place (spec D8): flash notices,
//! form errors, error pages. One register throughout - lowercase fragments,
//! `·` between parts, `→` for a link - so a copy pass is one diff here. Only
//! the privacy and imprint pages are exempt and keep full sentences.
//!
//! Fixed page text (headings, labels, rules) lives in the templates, which
//! follow the same register.

/// A flash notice by the code in `?msg=`: "ok" or "error", and the text.
/// Codes never carry visitor input, so nothing typed is echoed back.
pub fn notice(code: &str) -> Option<(&'static str, &'static str)> {
    Some(match code {
        // guessing
        "saved" => ("ok", "locked"),
        "already" => ("error", "already locked · can't change"),
        "empty" => ("error", "type a number first"),
        "invalid" => ("error", "whole numbers only"),
        "too_small" => ("error", "smallest guess is 1"),
        "too_big" => ("error", "past 9 223 372 036 854 775 807 · aim lower"),
        "signin" => ("error", "sign in to play"),
        "unverified" => ("error", "confirm your email to play"),
        "held_unverified" => ("error", "confirm your email first · number not entered"),
        "broke" => ("error", "not enough points · more each day you visit"),
        "voided" => ("error", "round called off · no more guesses"),
        // signing in
        "welcome" => ("ok", "welcome · pick a number"),
        "verified" => ("ok", "email confirmed · pick a number"),
        "password_set" => ("ok", "new password saved · other sessions signed out"),
        "resent" => ("ok", "new link sent · valid 24h"),
        "resend_wait" => ("error", "links sent already · try in an hour"),
        "mail_failed" => ("error", "mail didn't go out · try later"),
        "signed_out" => ("ok", "signed out"),
        "deleted" => ("ok", "account deleted"),
        // account
        "linked" => ("ok", "login added"),
        "already_linked" => ("ok", "already yours"),
        "linked_elsewhere" => ("error", "that login belongs to another account"),
        "unlinked" => ("ok", "removed"),
        "renamed" => ("ok", "renamed"),
        "password_changed" => ("ok", "password changed · other sessions signed out"),
        "password_added" => ("ok", "added · open the link we sent"),
        // moderation and admin
        "banned" => ("ok", "banned"),
        "unbanned" => ("ok", "unbanned"),
        "adjusted" => ("ok", "points adjusted"),
        "called_off" => ("ok", "round called off · stakes refunded"),
        // bets
        "streak_claimed" => ("ok", "streak bonus claimed · keep it going"),
        "bet_placed" => ("ok", "bet placed · settled at 12:42"),
        "bet_twice" => ("error", "one bet per range a round"),
        "bet_cap" => ("error", "stake: a whole number from 1 up to your points"),
        "bet_over_cap" => ("error", "stake too big: past the cap a win pays no more, so it only risks more"),
        "bet_closed" => ("error", "bets closed · round over"),
        "bet_odds" => ("error", "odds moved · check and bet again"),
        "bet_market" => ("error", "pick a range: whole numbers from 1, low to high"),
        "bet_slow" => ("error", "too many bets · wait a minute"),
        "added" => ("ok", "added · counts"),
        "added_ghost" => ("ok", "added as ghost · can't win"),
        "cleared" => ("ok", "round cleared"),
        "not_demo" => ("error", "real 42 account · stand-ins only"),
        "bad_login" => ("error", "login: letters, digits and - only"),
        "bad_guess" => ("error", "not a whole number above 0"),
        _ => return None,
    })
}

// ------------------------------------------------------------ with values

pub fn too_late() -> String {
    "too late · round closed".to_string()
}

pub fn closed_during_signin(number: Option<&str>, next_close: &str) -> String {
    match number {
        Some(n) => format!("round closed while you signed in · {n} not entered · next round {next_close}"),
        None => format!("round closed while you signed in · not entered · next round {next_close}"),
    }
}

pub fn already_locked(number: &str) -> String {
    format!("already locked: {number}")
}

pub fn sign_in_to_lock(number: &str) -> String {
    format!("sign in to lock {number}")
}

pub fn too_many_tries(minutes: u64) -> String {
    format!("too many tries · wait {minutes} min")
}

pub fn rename_wait(date: &str) -> String {
    format!("next rename {date}")
}

pub fn provider_failed(provider: &str) -> String {
    format!("{provider} sign-in failed · try again")
}

pub fn provider_refused(provider: &str, detail: &str) -> String {
    format!("{provider} refused the sign-in · {detail}")
}

pub fn email_in_use(email: &str, provider: &str) -> String {
    format!("{email} already has an account · sign in the usual way, then add {provider} on your account page")
}

pub fn github_too_new(days: i64) -> String {
    format!("github accounts under {days} days can't sign up · use google or email")
}

// ---------------------------------------------------------- form errors

pub const USERNAME_LENGTH: &str = "username: 3 to 20 characters";
pub const USERNAME_CHARS: &str = "username: a-z, 0-9, - and _ only";
pub const USERNAME_START: &str = "username: start with a letter or digit";
pub const USERNAME_RESERVED: &str = "username reserved";
pub const USERNAME_TAKEN: &str = "username taken";
pub const EMAIL_BAD: &str = "not an email address";
pub const EMAIL_THROWAWAY: &str = "throwaway email · not allowed";
pub const EMAIL_TAKEN: &str = "email already has an account";
pub const PASSWORD_SHORT: &str = "password: at least 10 characters";
pub const PASSWORD_LONG: &str = "password: at most 128 characters";
pub const PASSWORD_SELF: &str = "password can't be your email or username";
pub const PASSWORD_COMMON: &str = "too common · pick another";
pub const PASSWORD_WRONG: &str = "not your current password";
pub const WRONG_LOGIN: &str = "wrong username, email or password";
pub const TOO_MANY: &str = "too many tries · try later";
pub const HUMAN_CHECK: &str = "human check failed · try again";
pub const SIGNUP_LIMIT: &str = "too many new accounts from here · try in an hour";
pub const LAST_LOGIN: &str = "your only way in · add another first";
pub const KEEP_42: &str = "your 42 round needs this login";
pub const NO_EMAIL_SIGNIN: &str = "email sign-in not available here";
pub const CONFIRM_USERNAME: &str = "type your username exactly";
pub const NEED_REASON: &str = "reason required";
pub const NEED_NOTE: &str = "note required · the player sees it";
pub const NO_SUCH_ACCOUNT: &str = "no account by that name";
pub const NOT_YOURSELF: &str = "can't ban yourself";
pub const AMOUNT_BAD: &str = "amount: whole number, negative to take away";
pub const AMOUNT_RANGE: &str = "amount: -1 000 000 to 1 000 000, not 0";
pub const ROUND_DATE: &str = "round: a date like 2026-10-21";
pub const ROUND_SETTLED: &str = "round settled already · adjust instead";

// ---------------------------------------------------------- error pages

pub const BROKE_HERE: &str = "couldn't load";
pub const NO_PAGE: &str = "no such page";
pub const NO_DAY: &str = "no closed round that day";
pub const NO_PROVIDER: &str = "that sign-in isn't available here";
pub const LINK_INCOMPLETE: &str = "sign-in link incomplete · start again";
pub const LINK_USED: &str = "sign-in link expired or used · start again";
pub const LINK_OTHER_SESSION: &str = "started in another session · start again from your account page";
pub const CANT_SIGN_IN: &str = "this account can't sign in";
pub const GOOGLE_NO_EMAIL: &str = "google confirmed no email · can't use this account";
pub const DEAD_LINK: &str = "link expired or used";

#[cfg(test)]
mod tests {
    use super::*;

    /// The register: lowercase, no em-dashes, no sentences strung together.
    #[test]
    fn every_message_is_a_lowercase_fragment() {
        let codes = [
            "saved", "already", "empty", "invalid", "too_small", "too_big", "signin", "unverified",
            "held_unverified", "broke", "voided", "welcome", "verified", "password_set", "resent",
            "resend_wait", "mail_failed", "signed_out", "deleted", "linked", "already_linked",
            "linked_elsewhere", "unlinked", "renamed", "password_changed", "password_added", "banned",
            "unbanned", "adjusted", "called_off", "added", "added_ghost", "cleared", "not_demo",
            "bad_login", "bad_guess", "bet_placed", "bet_twice", "bet_cap", "bet_closed", "bet_odds",
            "bet_market", "bet_slow", "streak_claimed", "bet_over_cap",
        ];
        let mut all: Vec<String> = codes.iter().map(|c| notice(c).expect(c).1.to_string()).collect();
        all.extend([
            too_late(), closed_during_signin(Some("7"), "12:42"), already_locked("7"), sign_in_to_lock("7"),
            too_many_tries(3), USERNAME_LENGTH.into(), WRONG_LOGIN.into(), PASSWORD_COMMON.into(),
            EMAIL_THROWAWAY.into(), DEAD_LINK.into(),
        ]);
        for m in all {
            assert!(!m.contains('—'), "{m}");
            assert!(!m.ends_with('.'), "{m}");
            assert!(!m.contains(". "), "no strung sentences: {m}");
            assert!(!m.starts_with(|c: char| c.is_uppercase()), "{m}");
        }
        assert_eq!(too_late(), "too late · round closed");
        assert_eq!(
            closed_during_signin(Some("7"), "12:42"),
            "round closed while you signed in · 7 not entered · next round 12:42"
        );
        assert_eq!(sign_in_to_lock("7"), "sign in to lock 7");
        assert_eq!(already_locked("7"), "already locked: 7");
    }
}
