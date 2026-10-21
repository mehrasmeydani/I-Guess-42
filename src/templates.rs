use askama::Template;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};

use crate::db::{GuessRow, LeaderboardRow, Pool, RoundSummary, User, Viewer};
use crate::round;
use crate::stats::{self, Chart, Tally, Trend};

/// A fingerprint of the stylesheet and script, put in their URLs
/// (`style.css?v=...`) so a browser can never pair a new page with an old
/// cached stylesheet. Computed at compile time; `include_bytes!` also makes
/// cargo rebuild whenever either file changes.
pub const ASSET_VERSION: u64 = {
    let css = fnv1a(include_bytes!("../static/style.css"), 0xcbf2_9ce4_8422_2325);
    fnv1a(include_bytes!("../static/app.js"), css)
};

/// Settings the templates need on every page and that never change while the
/// process runs. Set once at startup; tests get the defaults.
pub struct Site {
    /// For absolute URLs in link previews: `https://iguesslow.com`.
    pub public_url: String,
    /// Keys the anonymous labels (see `anon_label`).
    pub anon_key: String,
}

static SITE: std::sync::OnceLock<Site> = std::sync::OnceLock::new();

pub fn init_site(site: Site) {
    let _ = SITE.set(site);
}

fn site() -> &'static Site {
    SITE.get_or_init(|| Site {
        public_url: "http://localhost:3000".to_string(),
        anon_key: "test".to_string(),
    })
}

/// The site's address, for `og:url` and `og:image`.
pub fn public_url() -> &'static str {
    &site().public_url
}

/// What is running, printed in the footer: the contents of version.txt,
/// bumped in the commit that gets tagged and deployed, so the footer and the
/// git tag cannot drift apart.
///
/// Its own file rather than Cargo.toml's `version`, and not for tidiness:
/// the Dockerfile builds the dependencies in a layer keyed on Cargo.toml, so
/// touching that file for a release throws away 281 compiled crates and adds
/// minutes to every deploy. version.txt is copied after that layer.
pub fn version() -> &'static str {
    include_str!("../version.txt").trim()
}

/// FNV-1a, 64-bit: tiny, and good enough to notice that a file changed.
const fn fnv1a(bytes: &[u8], mut hash: u64) -> u64 {
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
        i += 1;
    }
    hash
}

/// Insert thin separators every three digits, so a 19-digit guess is readable.
pub fn group_digits(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('\u{202f}'); // narrow no-break space
        }
        out.push(ch);
    }
    out
}

pub struct UserView {
    pub login: String,
    pub display_name: String,
    /// Plays the 42 round (a 42 student, or a stand-in on a test instance),
    /// and so may be shown that it exists.
    pub plays_42: bool,
    /// Their points in the global round, for the header there; `None` for an
    /// account that cannot play it yet.
    pub points: Option<i64>,
}

impl From<User> for UserView {
    fn from(u: User) -> Self {
        Self {
            login: u.login,
            display_name: u.display_name,
            plays_42: true,
            points: None,
        }
    }
}

/// Whether the page may mention the 42 round at all: the pool switch, the
/// word "global". Only for someone who plays it.
pub fn sees_42(user: &Option<UserView>) -> bool {
    user.as_ref().is_some_and(|u| u.plays_42)
}

/// A 42 student goes by their login, as everywhere else on the site; anyone
/// else by their username.
impl From<Viewer> for UserView {
    fn from(v: Viewer) -> Self {
        let points = v.account.verified.then_some(v.points);
        match v.intra {
            Some(u) => Self { points, ..Self::from(u) },
            None => Self {
                login: v.account.username.clone(),
                display_name: v.account.username,
                plays_42: false,
                points,
            },
        }
    }
}

/// The round's text bar, "[###-------]", 30 cells: short enough for the
/// narrowest phone; app.js widens it to the row.
pub fn meter(pct: &i64) -> String {
    const CELLS: usize = 30;
    let filled = (*pct.clamp(&0, &100) as usize) * CELLS / 100;
    format!("[{}{}]", "#".repeat(filled), "-".repeat(CELLS - filled))
}

/// A date the European way: "2026-10-21" is shown as "21.10.2026". Dates
/// stay ISO everywhere else (URLs, the database, sorting); this is only
/// how a page shows one. Anything that is not a date is shown as it is.
pub fn eu(iso: &str) -> String {
    match chrono::NaiveDate::parse_from_str(iso, "%Y-%m-%d") {
        Ok(d) => d.format("%d.%m.%Y").to_string(),
        Err(_) => iso.to_string(),
    }
}

/// A span of days, short: "14.–16.10.", "28.09.–03.10.", or "14.10." for
/// one day; with years when the span crosses one.
pub fn eu_span(from: &str, to: &str) -> String {
    let parse = |d: &str| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok();
    let (Some(a), Some(b)) = (parse(from), parse(to)) else {
        return format!("{from}–{to}");
    };
    use chrono::Datelike;
    if a == b {
        a.format("%d.%m.").to_string()
    } else if a.year() != b.year() {
        format!("{}–{}", a.format("%d.%m.%Y"), b.format("%d.%m.%Y"))
    } else if a.month() == b.month() {
        format!("{}–{}", a.format("%d."), b.format("%d.%m."))
    } else {
        format!("{}–{}", a.format("%d.%m."), b.format("%d.%m."))
    }
}

/// A search typed the European way, as the ISO prefix it means:
/// "21.10.2026" is "2026-10-21", "10.2026" is "2026-10"; anything else is
/// returned as it is.
pub fn iso_prefix(q: &str) -> String {
    let parts: Vec<&str> = q.split('.').map(str::trim).filter(|p| !p.is_empty()).collect();
    let num = |p: &str, len: usize| p.len() <= len && p.chars().all(|c| c.is_ascii_digit());
    match parts.as_slice() {
        [d, m, y] if num(d, 2) && num(m, 2) && y.len() == 4 && num(y, 4) => {
            format!("{y}-{:0>2}-{:0>2}", m, d)
        }
        [m, y] if num(m, 2) && y.len() == 4 && num(y, 4) => format!("{y}-{:0>2}", m),
        _ => q.to_string(),
    }
}

/// "1 250 pts".
pub fn points_label(points: &i64) -> String {
    format!("{} pts", group_digits(*points))
}

/// What a player is called for a visitor who may not see their name:
/// "player #3fa2", from an HMAC of their id. The same player gets the same
/// label every day and on every page, so their wins can be followed, but it
/// cannot be turned back into who they are (D6).
pub fn anon_label(id: i64) -> String {
    use hmac::{Hmac, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(site().anon_key.as_bytes())
        .expect("HMAC takes keys of any length");
    mac.update(&id.to_le_bytes());
    let bytes = mac.finalize().into_bytes();
    format!("player #{:02x}{:02x}", bytes[0], bytes[1])
}

/// The prefix every anonymous label starts with.
#[cfg(test)]
pub const MASK: &str = "player #";

/// A login or display name, shown only to a visitor who may see who played,
/// and an anonymous label otherwise. Everything that carries a player's
/// identity goes through here, so hiding it is not something a template can
/// forget to do.
fn named(value: String, id: Option<i64>, reveal: bool) -> String {
    match (reveal, id) {
        (true, _) => value,
        (false, Some(id)) => anon_label(id),
        (false, None) => "a player".to_string(),
    }
}

pub struct WinnerView {
    pub login: String,
    pub display_name: String,
    pub value_label: String,
}

pub struct RoundView {
    /// `YYYY-MM-DD`, for linking to the day's page.
    pub date_key: String,
    pub date_label: String,
    pub total: i64,
    pub winner: Option<WinnerView>,
}

impl RoundView {
    /// `reveal` is whether this visitor may be shown who won; a winner's
    /// login and name are replaced with [`MASK`] when they may not. There is
    /// deliberately no `From<RoundSummary>`: every caller has to answer.
    pub fn new(r: RoundSummary, reveal: bool) -> Self {
        // The winner columns are filled in together or not at all: a round
        // only has a winner if some value was picked exactly once.
        let winner = match (r.winner_login, r.winner_name, r.winning_value) {
            (Some(login), Some(display_name), Some(value)) => Some(WinnerView {
                login: named(login, r.winner_id, reveal),
                display_name: named(display_name, r.winner_id, reveal),
                value_label: group_digits(value),
            }),
            _ => None,
        };
        Self {
            date_label: round::format_round_date(&r.round_date),
            date_key: r.round_date,
            total: r.total,
            winner,
        }
    }
}

pub struct LeaderView {
    pub login: String,
    pub display_name: String,
    pub wins: i64,
    /// Place in the ranking; ties share it (see [`ranks`]).
    pub rank: i64,
}

/// Places for scores in descending order, the way sport ranks them: equal
/// scores share a place and the next place skips past them, so nine players
/// tied first are all 1 and the next is 10.
pub fn ranks(scores: &[i64]) -> Vec<i64> {
    let mut out = Vec::with_capacity(scores.len());
    for (i, s) in scores.iter().enumerate() {
        let rank = if i > 0 && scores[i - 1] == *s { out[i - 1] } else { i as i64 + 1 };
        out.push(rank);
    }
    out
}

impl LeaderView {
    /// As [`RoundView::new`]: `reveal` decides whether the player is named or
    /// masked. No `From<LeaderboardRow>`, for the same reason.
    pub fn new(r: LeaderboardRow, reveal: bool) -> Self {
        Self {
            login: named(r.login, Some(r.id), reveal),
            display_name: named(r.display_name, Some(r.id), reveal),
            wins: r.wins,
            rank: 0,
        }
    }
}

/// A one-shot banner above the guess form.
pub struct Notice {
    pub kind: &'static str, // "ok" or "error"
    pub text: String,
}

impl Notice {
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            kind: "ok",
            text: text.into(),
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            kind: "error",
            text: text.into(),
        }
    }
}

#[derive(Template)]
#[template(path = "index.html")]
pub struct IndexTemplate {
    /// Which game the page is about; picks link prefixes and pool-only blocks.
    pub pool: Pool,
    /// Streak bonus points waiting to be claimed, "1 000 pts": the pop-up.
    pub streak_bonus: Option<String>,
    pub user: Option<UserView>,
    pub test_mode: bool,
    /// Browsing as a demo account, with an admin session parked to return to.
    pub impersonating: bool,
    /// `YYYY-MM-DD`, round-tripped through the form's hidden field.
    pub round_key: String,
    /// Pre-formatted ("18h 32m"), so the page is right before the countdown
    /// script runs, and the same for a screen reader.
    pub time_left: String,
    pub time_left_spoken: String,
    /// The deadline and the server's clock as RFC 3339, for app.js.
    pub closes_at: String,
    pub server_now: String,
    /// The deadline on the Vienna clock, "12:42".
    pub close_time: String,
    /// How much of the round has elapsed, 0-100.
    pub progress_pct: i64,
    pub my_guess_label: Option<String>,
    /// The guess was locked just now: show the confirmation panel.
    pub just_locked: bool,
    pub signed_in: bool,
    /// Where the .ics for this round's reveal is.
    pub calendar_href: String,
    /// The player's record, for the line under the status block.
    pub stats: Option<crate::stats::PlayerStats>,
    /// The global round's pot so far, and how much of it rolled over from
    /// rounds nobody won.
    pub pot_label: Option<String>,
    pub carried_label: Option<String>,
    /// What a guess costs, in the global round.
    pub stake_label: Option<String>,
    /// A few of today's odds, for the line that leads to the bets page.
    pub bet_teaser: Option<String>,
    pub guess_count: i64,
    pub last_round: Option<RoundView>,
    /// How much of the announcement window is left, in seconds, or `None`
    /// once it is over. While it is `Some`, the round in `last_round` is
    /// announced over the page. Counted on the server, like `seconds_left`:
    /// a visitor whose clock is wrong must not get their own idea of when
    /// 12:42 was.
    pub announce_left: Option<i64>,
    /// Whether to print the line about coalition points under the rules. Only
    /// for a signed-in player, and off on an instance that pays none, so the
    /// page never promises points that are not coming; see
    /// `Config::points_announced`.
    pub points_announced: bool,
    pub notice: Option<Notice>,
    /// Whether the visitor may guess in this pool: a 42 player in the 42
    /// pool, a verified account in the global one. Signed in is not enough.
    pub can_play: bool,
}

/// The "are you sure?" step. Carries the value in both a display form and a
/// canonical raw form for the hidden field.
#[derive(Template)]
#[template(path = "confirm.html")]
pub struct ConfirmTemplate {
    /// Which game the page is about; picks link prefixes and pool-only blocks.
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    /// Browsing as a demo account, with an admin session parked to return to.
    pub impersonating: bool,
    pub value_label: String,
    pub value_raw: String,
    pub round_key: String,
    pub deadline_human: String,
    /// What locking it costs, in the global round.
    pub stake_label: Option<String>,
}

#[derive(Template)]
#[template(path = "results.html")]
pub struct ResultsTemplate {
    /// Which game the page is about; picks link prefixes and pool-only blocks.
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    /// Browsing as a demo account, with an admin session parked to return to.
    pub impersonating: bool,
    /// The round that closed most recently, shown above everything else.
    /// `None` only until a first round has closed.
    pub latest: Option<RoundView>,
    /// Where the numbers come from: test, seeded or historical.
    pub data_source: &'static str,
    /// When the open round closes, for the page before any round has.
    pub first_close: String,
    /// The last round's chart; `Some` exactly when `latest` is. Named
    /// `chart` because chart.html reads that binding.
    pub chart: Option<Chart>,
    /// The rounds to list: the most recent few, or every match of a search.
    pub rounds: Vec<RoundView>,
    /// How many closed rounds exist in all.
    pub total_rounds: usize,
    /// The search as typed, echoed back into the box.
    pub q: String,
    /// "7", "30" or "all".
    pub show: &'static str,
    /// Whether this visitor may see who played. Only the search box reads it:
    /// the names themselves are already masked by the time they get here.
    pub names_visible: bool,
    /// The top of the leaderboard, always shown.
    pub leaders: Vec<LeaderView>,
    /// Everyone else, folded away.
    pub rest: Vec<LeaderView>,
}

/// One row of the "most" / "least picked" lists and the full table.
pub struct TallyView {
    pub value_label: String,
    pub count: i64,
}

impl From<&Tally> for TallyView {
    fn from(t: &Tally) -> Self {
        Self {
            value_label: group_digits(t.value),
            count: t.count,
        }
    }
}

/// A closed round, laid open: the distribution chart and the headline numbers.
#[derive(Template)]
#[template(path = "day.html")]
pub struct DayTemplate {
    /// Which game the page is about; picks link prefixes and pool-only blocks.
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    /// Browsing as a demo account, with an admin session parked to return to.
    pub impersonating: bool,
    pub round: RoundView,
    pub distinct: usize,
    pub chart: Chart,
    /// The rounds before this one, newest first, so the calendar is reachable
    /// from the round itself instead of only from /results.
    pub earlier: Vec<RoundView>,
    /// The round after this one, `None` on the newest closed round. Without it
    /// the list below would only ever walk backwards.
    pub newer: Option<String>,
    /// How many closed rounds there are in all, to say what the list leaves out.
    pub total_rounds: i64,
    /// The closed round before this one, for the "← date" link.
    pub previous: Option<String>,
    /// "5 won · 41 players · 1, 2, 3, 4 were all taken".
    pub takeaway: String,
    /// The chart said in words, for anyone who cannot see it.
    pub summary: String,
    /// Where the numbers come from: test, seeded, historical or live.
    pub data_source: &'static str,
    /// The visitor's own pick that day, and how it went, if they played.
    pub my_pick: Option<String>,
    /// The text "share" copies, for a player who played that day. Never
    /// contains their own number.
    pub share_text: Option<String>,
    /// Every picked number with what became of it, for the table.
    pub rows: Vec<PickRow>,
    /// The global round's pot that day, in words: "the pot of 340 pts went
    /// to the winner", "nobody won: 340 pts rolled over".
    pub pot_line: Option<String>,
}

/// A number in a day's table: how many picked it, and its fate.
pub struct PickRow {
    pub value_label: String,
    pub count: i64,
    /// "won", "unique" (picked once, not the lowest) or "taken".
    pub fate: &'static str,
}

/// What `share` copies (D7): the day, how the player's pick did, the
/// winning number and headcount, their streak if it is worth mentioning,
/// and the link. `outcome` says how they did without saying what they
/// picked, so the text can be posted without giving the number away.
pub fn share_text(date: &str, outcome: &str, winner: Option<i64>, players: i64, streak: i64, url: &str) -> String {
    let day = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map(|d| d.format("%b %-d").to_string().to_lowercase())
        .unwrap_or_else(|_| date.to_string());
    let winner = winner.map_or_else(|| "nobody".to_string(), group_digits);
    let mut text = format!("i guess low · {day}\nmy pick: {outcome}\nwinner: {winner} · {players} players\n");
    if streak >= 2 {
        text.push_str(&format!("🔥 {streak}-day streak\n"));
    }
    text.push_str(url);
    text
}

/// How a pick did, for the share text: 🟩 won, 🟥 taken by others, or 🟨
/// picked by nobody else but not the lowest such number.
pub fn outcome_symbol(mine: i64, count: i64, winner: Option<i64>) -> String {
    if winner == Some(mine) {
        "🟩 won".to_string()
    } else if count > 1 {
        format!("🟥 (taken by {})", count - 1)
    } else {
        "🟨 unique, not lowest".to_string()
    }
}

/// A round on the "my rounds" page.
pub struct MyRoundView {
    pub date: String,
    pub pick: String,
    pub winner: Option<String>,
    pub won: bool,
}

/// `/g/me`: a player's record and every round they played (R14.2).
#[derive(Template)]
#[template(path = "me.html")]
pub struct MeTemplate {
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    pub impersonating: bool,
    pub stats: crate::stats::PlayerStats,
    pub best_label: Option<String>,
    /// Newest first; the first 30, then the rest folded away.
    pub rounds: Vec<MyRoundView>,
    pub older: Vec<MyRoundView>,
    /// Signed in, but not allowed to play this pool (unconfirmed email).
    pub cannot_play: bool,
}

/// The username step after a first sign-in through a provider.
#[derive(Template)]
#[template(path = "welcome.html")]
pub struct WelcomeTemplate {
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    pub impersonating: bool,
    pub username: String,
    pub then: String,
    pub error: Option<&'static str>,
}

/// A ready-made range on the bets board.
pub struct OfferView {
    pub key: String,
    pub label: String,
    /// "27%", or "0.4%" under ten.
    pub chance: String,
    /// None: too safe to offer.
    pub odds: Option<String>,
    /// The one picked above.
    pub on: bool,
    /// Bar length against the likeliest on the board, 0-100.
    pub pct: i64,
}

/// The range the visitor picked, priced.
pub struct QuoteView {
    pub key: String,
    pub label: String,
    /// "27%", or "0.4%" under ten.
    pub chance: String,
    /// None: too safe to offer.
    pub odds: Option<String>,
    pub odds_x100: i64,
    /// The chance in hundredths of a percent, for its bar.
    pub bp: i64,
    /// The most a bet on it can win over its stake today, and the stake that
    /// reaches it: the stake box takes no more.
    pub max_profit: i64,
    pub max_stake: i64,
    /// Players a day, the reason for the cap.
    pub players: usize,
    /// Bet on already today.
    pub taken: bool,
}

/// A bet, open or settled.
pub struct SlipView {
    pub date: String,
    pub market: String,
    pub stake: i64,
    pub odds: String,
    /// "open", "won", "lost" or "void".
    pub result: String,
    /// What it pays (open) or paid (settled), stake included.
    pub amount: i64,
    /// Settled: what it won or lost, net.
    pub net: Option<i64>,
}

/// Just the quote box, swapped into the bets page as the visitor types.
#[derive(Template)]
#[template(path = "_quote.html")]
pub struct QuoteTemplate {
    pub quote: Option<QuoteView>,
    pub pick_error: Option<&'static str>,
    pub can_bet: bool,
    pub round_key: String,
    pub tax: String,
}

#[derive(Template)]
#[template(path = "bets.html")]
pub struct BetsTemplate {
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    pub impersonating: bool,
    pub notice: Option<Notice>,
    pub round_key: String,
    pub close_time: String,
    pub signed_in: bool,
    pub can_bet: bool,
    /// The range boxes, as picked or typed.
    pub lo: String,
    pub hi: String,
    pub nobody_picked: bool,
    pub quote: Option<QuoteView>,
    /// Why what was typed is not a range.
    pub pick_error: Option<&'static str>,
    pub board: Vec<OfferView>,
    /// The same for the most picked number.
    pub board_most: Vec<OfferView>,
    /// The boxes are about the most picked number, not the winner.
    pub on_most: bool,
    /// Points on the book today, and how many bets.
    pub book_total: i64,
    pub book_bets: usize,
    /// Made-up rounds behind the odds; 0 if there were no picks to replay.
    pub sims: u32,
    /// Players a day on average, and so the most one bet can win over its
    /// stake ("3 000").
    pub players: usize,
    pub max_profit: String,
    /// "8%".
    pub tax: String,
    pub mine: Vec<SlipView>,
    pub past: Vec<SlipView>,
    /// Net over the listed past slips, "+40" or "-25".
    pub net_label: String,
}

/// A line of points history.
pub struct LedgerView {
    pub date: String,
    pub what: String,
    /// "+100", "-10".
    pub amount: String,
    pub gain: bool,
}

#[derive(Template)]
#[template(path = "points.html")]
pub struct PointsTemplate {
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    pub impersonating: bool,
    pub balance_label: String,
    pub rows: Vec<LedgerView>,
    pub stake: i64,
    pub grant: i64,
    pub bonus: i64,
}

pub struct PointsBoardRow {
    pub name: String,
    /// The name in full, for the row's tooltip.
    pub title: String,
    /// Points, or days won, as shown.
    pub points: String,
    /// Bar length against the top of the table, 0-100.
    pub pct: i64,
    /// The visitor's own row.
    pub me: bool,
    /// Place in the ranking; ties share it.
    pub rank: i64,
}

#[derive(Template)]
#[template(path = "leaderboard.html")]
pub struct BoardTemplate {
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    pub impersonating: bool,
    /// Gains over the last week, rather than balances.
    pub week: bool,
    /// Points, highest first: the global round only.
    pub rows: Vec<PointsBoardRow>,
    /// The most days won, all-time.
    pub wins: Vec<PointsBoardRow>,
}

/// One day on the trends timeline.
pub struct DayLineView {
    pub date: String,
    pub players: i64,
    /// Bar lengths as a share of the busiest day and the highest winner.
    pub players_pct: i64,
    pub winner_label: Option<String>,
    pub winner_pct: i64,
    /// Far above the day's other picks, and left out of the averages.
    pub winner_outlier: bool,
}

/// An older-half vs newer-half comparison, printed as `before -> after`.
pub struct ChangeView {
    pub label: &'static str,
    /// One line on what the row means, shown on hover.
    pub explain: &'static str,
    pub before: String,
    pub after: String,
    /// Signed difference, e.g. "+47.6"; empty if either side is missing.
    pub delta: String,
    /// "up", "down" or "same".
    pub dir: &'static str,
}

impl ChangeView {
    fn new(
        label: &'static str,
        explain: &'static str,
        before: Option<i64>,
        after: Option<i64>,
    ) -> Self {
        let show = |v: Option<i64>| v.map_or_else(|| "-".to_string(), stats::hundredths);
        let dir = match (before, after) {
            (Some(b), Some(a)) if a > b => "up",
            (Some(b), Some(a)) if a < b => "down",
            _ => "same",
        };
        let delta = match (before, after) {
            (Some(b), Some(a)) => stats::signed_hundredths(a - b),
            _ => String::new(),
        };
        Self {
            label,
            explain,
            before: show(before),
            after: show(after),
            delta,
            dir,
        }
    }
}

pub struct MoverView {
    pub value_label: String,
    pub before: String,
    pub after: String,
    /// The difference in percentage points, e.g. "+1.7 pts".
    pub delta: String,
}

/// Several closed rounds taken together: the summed spread, a timeline, and
/// what changed between the older and the newer half.
#[derive(Template)]
#[template(path = "trends.html")]
pub struct TrendsTemplate {
    /// Which game the page is about; picks link prefixes and pool-only blocks.
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    /// Browsing as a demo account, with an admin session parked to return to.
    pub impersonating: bool,
    /// "7", "30" or "all", as in the query string.
    pub range: &'static str,
    pub rounds: usize,
    /// Every pick in the range, added up.
    pub picks: i64,
    /// How long reading and analysing the range took, "0.4 ms".
    pub took: String,
    pub chart: Chart,
    /// Every value picked in the range with its summed count, lowest first.
    pub totals: Vec<TallyView>,
    /// Newest first, every day in the range.
    pub days: Vec<DayLineView>,
    pub changes: Vec<ChangeView>,
    /// The two halves being compared, as short date ranges.
    pub older_range: String,
    pub newer_range: String,
    pub rising: Vec<MoverView>,
    pub falling: Vec<MoverView>,
    /// The most picked numbers of the whole range.
    pub most: Vec<TallyView>,
    /// When the open round closes, for a range with nothing in it yet.
    pub first_close: String,
    /// The custom form's four dates, as shown: span 1 from/to, span 2 from/to.
    pub span_inputs: [String; 4],
    /// Why the custom dates could not be used.
    pub span_error: Option<&'static str>,
    /// What the two sides of "what changed" are called.
    pub older_label: &'static str,
    pub newer_label: &'static str,
}

impl TrendsTemplate {
    pub fn build(
        pool: Pool,
        user: Option<UserView>,
        test_mode: bool,
        impersonating: bool,
        range: &'static str,
        trend: &Trend,
    ) -> Self {
        let mover = |m: &stats::Mover| MoverView {
            value_label: group_digits(m.value),
            before: stats::permille(m.before_permille),
            after: stats::permille(m.after_permille),
            delta: stats::permille_points(m.after_permille - m.before_permille),
        };
        let date_span = |(from, to): &(String, String)| {
            // The year is the same on both sides almost always; leave it out.
            eu_span(from, to)
        };
        let (older_range, newer_range) = trend
            .halves
            .as_ref()
            .map_or_else(Default::default, |h| (date_span(&h.older), date_span(&h.newer)));
        let most_players = trend.days.iter().map(|d| d.players).max().unwrap_or(1).max(1);
        // An outlier winner would flatten every other day's bar to a sliver.
        let top_winner = trend
            .days
            .iter()
            .filter(|d| !d.winner_outlier)
            .filter_map(|d| d.winner)
            .max()
            .unwrap_or(1)
            .max(1);
        // Rounded up, so any non-zero value shows at least a sliver. In i128,
        // as winners reach i64::MAX; an outlier has no bar, but a big winner
        // that is not one is still measured.
        let share = |v: i64, of: i64| {
            let (v, of) = (i128::from(v), i128::from(of));
            ((v * 100 + of - 1) / of).min(100) as i64
        };
        let changes = trend.halves.as_ref().map_or_else(Vec::new, |h| {
            vec![
                ChangeView::new(
                    "players per day",
                    "average number of people who guessed each day",
                    Some(h.players.0),
                    Some(h.players.1),
                ),
                ChangeView::new(
                    "winning number",
                    "average of the numbers that won; days with no winner, and outliers far above the day's other picks, are skipped",
                    h.winner.0,
                    h.winner.1,
                ),
                ChangeView::new(
                    "lowest free number",
                    "average lowest number nobody picked - the easiest win there was",
                    Some(h.lowest_free.0),
                    Some(h.lowest_free.1),
                ),
            ]
        });
        Self {
            pool,
            user,
            test_mode,
            impersonating,
            range,
            rounds: trend.days.len(),
            picks: trend.totals.iter().map(|t| t.count).sum(),
            took: String::new(),
            chart: stats::range_chart(trend),
            totals: trend.totals.iter().map(Into::into).collect(),
            days: trend
                .days
                .iter()
                .rev()
                .map(|d| DayLineView {
                    date: d.date.clone(),
                    players: d.players,
                    players_pct: share(d.players, most_players),
                    winner_label: d.winner.map(group_digits),
                    winner_pct: d.winner.map_or(0, |w| share(w, top_winner)),
                    winner_outlier: d.winner_outlier,
                })
                .collect(),
            changes,
            older_range,
            newer_range,
            rising: trend.rising.iter().map(mover).collect(),
            falling: trend.falling.iter().map(mover).collect(),
            most: trend.most.iter().map(Into::into).collect(),
            first_close: String::new(),
            span_inputs: Default::default(),
            span_error: None,
            older_label: "first half",
            newer_label: "second half",
        }
    }
}

pub struct AdminGuessView {
    pub login: String,
    pub display_name: String,
    pub value_label: String,
    /// Entered from /admin to try the round out; excluded from the headcount
    /// and from deciding a winner.
    pub ghost: bool,
}

impl From<GuessRow> for AdminGuessView {
    fn from(g: GuessRow) -> Self {
        Self {
            login: g.login,
            display_name: g.display_name,
            value_label: group_digits(g.value),
            ghost: !g.participates,
        }
    }
}

/// One pool on the admin page: its form value, its name, its open round.
pub struct AdminPoolView {
    pub key: &'static str,
    pub label: &'static str,
    pub guesses: Vec<AdminGuessView>,
}

#[derive(Template)]
#[template(path = "admin.html")]
pub struct AdminTemplate {
    /// Which game the page is about; picks link prefixes and pool-only blocks.
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    /// Browsing as a demo account, with an admin session parked to return to.
    pub impersonating: bool,
    pub real_now: String,
    pub game_now: String,
    pub clock_offset: String,
    pub round_key: String,
    pub round_label: String,
    pub deadline_human: String,
    pub time_left: String,
    /// Each pool's open round, which players are not allowed to see.
    pub pools: Vec<AdminPoolView>,
    /// Existing stand-in accounts, offered as one-click sign-ins.
    pub demo_users: Vec<UserView>,
    /// Generated history: (bot players, their guesses).
    pub demo_counts: (i64, i64),
    pub last_round: Option<RoundView>,
}

#[derive(Template)]
#[template(path = "error.html")]
pub struct ErrorTemplate {
    /// Which game the page is about; picks link prefixes and pool-only blocks.
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    /// Browsing as a demo account, with an admin session parked to return to.
    pub impersonating: bool,
    pub status: u16,
    pub message: String,
}

/// What was wrong with a submitted form: a summary for the top, which takes
/// focus, and a line for each field it concerns (R17.5).
#[derive(Default)]
pub struct FormErrors {
    pub summary: Option<String>,
    pub email: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    /// The email-or-username field on the sign-in form.
    pub login: Option<String>,
}

impl FormErrors {
    /// One problem, with the field it belongs to: "email", "username",
    /// "password", "login", or "" for one that concerns the whole form.
    pub fn on(field: &str, message: impl Into<String>) -> Self {
        let message = message.into();
        let mut e = FormErrors {
            summary: Some(message.clone()),
            ..FormErrors::default()
        };
        match field {
            "email" => e.email = Some(message),
            "username" => e.username = Some(message),
            "password" => e.password = Some(message),
            "login" => e.login = Some(message),
            _ => {}
        }
        e
    }
}

/// The sign-in page: every way in that this instance offers.
#[derive(Template)]
#[template(path = "login.html")]
pub struct LoginTemplate {
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    pub impersonating: bool,
    pub notice: Option<Notice>,
    pub errors: FormErrors,
    /// Typed into the email-or-username field last time, so a wrong password
    /// does not cost retyping it.
    pub email: String,
    pub email_signup: bool,
    pub github: bool,
    pub google: bool,
}

#[derive(Template)]
#[template(path = "signup.html")]
pub struct SignupTemplate {
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    pub impersonating: bool,
    pub notice: Option<Notice>,
    pub errors: FormErrors,
    pub enabled: bool,
    pub email: String,
    pub username: String,
    pub turnstile_site_key: Option<String>,
}

/// A page that only says something: "check your inbox", "that link expired".
#[derive(Template)]
#[template(path = "message.html")]
pub struct MessageTemplate {
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    pub impersonating: bool,
    pub title: &'static str,
    pub lines: Vec<String>,
    /// Where to go next, and what the button says.
    pub link: Option<(&'static str, &'static str)>,
}

#[derive(Template)]
#[template(path = "forgot.html")]
pub struct ForgotTemplate {
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    pub impersonating: bool,
    pub notice: Option<Notice>,
}

#[derive(Template)]
#[template(path = "reset.html")]
pub struct ResetTemplate {
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    pub impersonating: bool,
    pub notice: Option<Notice>,
    pub token: String,
}

#[derive(Template)]
#[template(path = "verify.html")]
pub struct VerifyTemplate {
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    pub impersonating: bool,
    pub token: String,
}

/// One way of signing in on the account page.
pub struct LoginView {
    /// The provider key, for the link and unlink forms.
    pub key: &'static str,
    pub name: &'static str,
    pub linked: bool,
    /// What it signs in as: an email, a 42 login.
    pub detail: String,
    /// Offered on this instance, so it can be added.
    pub available: bool,
    pub can_unlink: bool,
    /// Why it cannot be removed, when it cannot.
    pub why_not: &'static str,
}

#[derive(Template)]
#[template(path = "account.html")]
pub struct AccountTemplate {
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    pub impersonating: bool,
    pub notice: Option<Notice>,
    pub account_id: i64,
    pub username: String,
    pub plays_42: bool,
    pub can_rename: bool,
    pub rename_hint: String,
    pub logins: Vec<LoginView>,
    pub password_email: Option<String>,
    pub password_verified: bool,
    pub email_signup: bool,
}

#[derive(Template)]
#[template(path = "mod.html")]
pub struct ModTemplate {
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    pub impersonating: bool,
    pub notice: Option<Notice>,
    pub clusters: Vec<crate::db::Cluster>,
    pub banned: Vec<crate::db::ModAccount>,
}

#[derive(Template)]
#[template(path = "privacy.html")]
pub struct PrivacyTemplate {
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    pub impersonating: bool,
}

#[derive(Template)]
#[template(path = "imprint.html")]
pub struct ImprintTemplate {
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    pub impersonating: bool,
    /// Who runs the site, as a sentence: see `operator_sentence`.
    pub operator: String,
}

/// The imprint's one sentence, from whatever of name, address and contact
/// is configured. A missing part is left out of the sentence rather than
/// shown as a placeholder.
pub fn operator_sentence(name: Option<&str>, address: Option<&str>, email: Option<&str>) -> String {
    let who = match (name, address) {
        (Some(n), Some(a)) => format!("i guess low is run by {n}, {a}."),
        (Some(n), None) => format!("i guess low is run by {n}."),
        (None, Some(a)) => format!("i guess low is run from {a}."),
        (None, None) => "i guess low is run by students, for students.".to_string(),
    };
    match email {
        Some(e) => format!("{who} contact: {e}"),
        None => who,
    }
}

/// Render a template, or fall back to a bare 500 if the template itself is broken.
pub fn render<T: Template>(template: &T) -> Response {
    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(err) => {
            tracing::error!(%err, "template rendering failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "template rendering failed",
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{group_digits, TrendsTemplate};
    use crate::db::Pool;
    use crate::stats::{trend, DayTally};
    use askama::Template;

    #[test]
    fn an_outlier_winner_is_listed_without_a_bar() {
        // A normal day won on 2, and a day where 1..=30 all collided and one
        // player alone on 2 147 483 648 won. A bar for that would be off any
        // scale the other days share, so it gets none.
        let mut rows: Vec<DayTally> = [(1, 2), (2, 1)]
            .into_iter()
            .map(|(value, count)| DayTally {
                round_date: "2026-10-07".into(),
                value,
                count,
            })
            .collect();
        rows.extend((1..=30).map(|value| DayTally {
            round_date: "2026-10-08".into(),
            value,
            count: 2,
        }));
        rows.push(DayTally {
            round_date: "2026-10-08".into(),
            value: 2_147_483_648,
            count: 1,
        });
        let html = TrendsTemplate::build(Pool::Vienna, None, false, false, "7", &trend(&rows))
            .render()
            .unwrap();

        let row = |date: &str| {
            let from = html.find(&format!(">{}<", super::eu(date))).expect("a row for the day");
            let row = &html[from..];
            row[..row.find("</tr>").unwrap()].to_string()
        };
        let wild = row("2026-10-08");
        assert!(wild.contains("outlier"));
        assert_eq!(
            wild.matches("minibar").count(),
            1,
            "only the players bar: {wild}"
        );
        assert_eq!(
            row("2026-10-07").matches("minibar").count(),
            2,
            "a normal day keeps both"
        );
    }

    #[test]
    fn ties_share_a_place_and_the_next_place_skips_past_them() {
        use super::ranks;
        assert_eq!(ranks(&[5, 3, 3, 1]), vec![1, 2, 2, 4]);
        assert_eq!(ranks(&[2; 9].iter().copied().chain([1]).collect::<Vec<_>>()), vec![1, 1, 1, 1, 1, 1, 1, 1, 1, 10]);
        assert_eq!(ranks(&[7, 7, 6, 6, 6, 2]), vec![1, 1, 3, 3, 3, 6]);
        assert!(ranks(&[]).is_empty());
    }

    #[test]
    fn dates_are_shown_the_european_way() {
        use super::{eu, eu_span, iso_prefix};
        assert_eq!(eu_span("2026-10-14", "2026-10-16"), "14.–16.10.");
        assert_eq!(eu_span("2026-09-28", "2026-10-03"), "28.09.–03.10.");
        assert_eq!(eu_span("2026-12-30", "2027-01-02"), "30.12.2026–02.01.2027");
        assert_eq!(eu_span("2026-10-14", "2026-10-14"), "14.10.");
        assert_eq!(eu("2026-10-21"), "21.10.2026");
        assert_eq!(eu("2026-01-05"), "05.01.2026");
        assert_eq!(eu("not a date"), "not a date");
        assert_eq!(iso_prefix("21.10.2026"), "2026-10-21");
        assert_eq!(iso_prefix("5.1.2026"), "2026-01-05");
        assert_eq!(iso_prefix("10.2026"), "2026-10");
        assert_eq!(iso_prefix("1337"), "1337");
        assert_eq!(iso_prefix("megardes"), "megardes");
    }

    #[test]
    fn groups_digits_in_threes() {
        assert_eq!(group_digits(7), "7");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1_000), "1\u{202f}000");
        assert_eq!(group_digits(1_234_567), "1\u{202f}234\u{202f}567");
        assert_eq!(
            group_digits(i64::MAX),
            "9\u{202f}223\u{202f}372\u{202f}036\u{202f}854\u{202f}775\u{202f}807"
        );
    }
}
