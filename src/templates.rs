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
}

impl From<User> for UserView {
    fn from(u: User) -> Self {
        Self {
            login: u.login,
            display_name: u.display_name,
            plays_42: true,
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
        match v.intra {
            Some(u) => Self::from(u),
            None => Self {
                login: v.account.username.clone(),
                display_name: v.account.username,
                plays_42: false,
            },
        }
    }
}

/// What stands in for a login or a real name when the visitor is not allowed
/// to see it.
pub const MASK: &str = "*****";

/// A login or display name, shown only to a visitor who may see who played.
/// Everything that carries a student's identity goes through here, so hiding
/// it is not something a template can forget to do.
fn named(value: String, reveal: bool) -> String {
    if reveal { value } else { MASK.to_string() }
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
                login: named(login, reveal),
                display_name: named(display_name, reveal),
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
}

impl LeaderView {
    /// As [`RoundView::new`]: `reveal` decides whether the player is named or
    /// masked. No `From<LeaderboardRow>`, for the same reason.
    pub fn new(r: LeaderboardRow, reveal: bool) -> Self {
        Self {
            login: named(r.login, reveal),
            display_name: named(r.display_name, reveal),
            wins: r.wins,
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
    pub user: Option<UserView>,
    pub test_mode: bool,
    /// Browsing as a demo account, with an admin session parked to return to.
    pub impersonating: bool,
    /// `YYYY-MM-DD`, round-tripped through the form's hidden field.
    pub round_key: String,
    pub seconds_left: i64,
    /// Pre-formatted, so the page is right before the countdown script runs.
    pub time_left: String,
    /// How much of the round has elapsed, 0-100, and the same as a text bar.
    pub progress_pct: i64,
    pub progress_bar: String,
    pub my_guess_label: Option<String>,
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
    /// That round's chart. `Some` exactly when `latest` is; the two are kept
    /// apart because chart.html reads a binding called `chart`.
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
    pub lowest_unpicked_label: String,
    pub most: Vec<TallyView>,
    /// Every picked value, for readers who want the numbers rather than the chart.
    pub all: Vec<TallyView>,
    pub chart: Chart,
    /// The rounds before this one, newest first, so the calendar is reachable
    /// from the round itself instead of only from /results.
    pub earlier: Vec<RoundView>,
    /// The round after this one, `None` on the newest closed round. Without it
    /// the list below would only ever walk backwards.
    pub newer: Option<String>,
    /// How many closed rounds there are in all, to say what the list leaves out.
    pub total_rounds: i64,
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
    pub picks: i64,
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
    /// How long reading and analysing the range took, e.g. "12.4 ms".
    pub took: String,
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
            let short = |d: &str| d.get(5..).unwrap_or(d).to_string();
            if from == to {
                short(from)
            } else {
                format!("{} to {}", short(from), short(to))
            }
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
            took: String::new(),
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

/// The sign-in page: every way in that this instance offers.
#[derive(Template)]
#[template(path = "login.html")]
pub struct LoginTemplate {
    pub pool: Pool,
    pub user: Option<UserView>,
    pub test_mode: bool,
    pub impersonating: bool,
    pub notice: Option<Notice>,
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
    pub name: Option<String>,
    pub address: Option<String>,
    pub email: Option<String>,
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
            let from = html.find(&format!(">{date}<")).expect("a row for the day");
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
